//! Passwords are read from stdin/terminal, never process arguments.
use crate::{Config, Error, Result, users::Users};
use std::io::{BufRead, IsTerminal, Write};
use zeroize::Zeroizing;
const USAGE: &str = "usage: syncserver users create NAME [--password-stdin] [--adopt-legacy] | list | password NAME [--password-stdin] | disable NAME | enable NAME";
struct Echo(libc::termios);
impl Echo {
    fn disable() -> Result<Self> {
        // SAFETY: initialized termios and the current process's stdin descriptor.
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut original) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut hidden = original;
        hidden.c_lflag &= !(libc::ECHO | libc::ECHONL | libc::ICANON | libc::ISIG);
        hidden.c_cc[libc::VMIN] = 1;
        hidden.c_cc[libc::VTIME] = 0;
        // SAFETY: stdin is a terminal and hidden came from tcgetattr.
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &hidden) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Self(original))
    }
}
impl Drop for Echo {
    fn drop(&mut self) {
        // SAFETY: restore the saved attributes on the same process descriptor.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.0);
        }
    }
}
fn line(prompt: &str, terminal: bool) -> Result<Zeroizing<String>> {
    if terminal {
        eprint!("{prompt}");
        std::io::stderr().flush()?;
    }
    let guard = if terminal {
        Some(Echo::disable()?)
    } else {
        None
    };
    let mut secret = if terminal {
        use std::io::Read;
        let mut bytes = Zeroizing::new(Vec::new());
        let mut stdin = std::io::stdin().lock();
        loop {
            let mut byte = [0u8; 1];
            stdin.read_exact(&mut byte)?;
            match byte[0] {
                b'\n' | b'\r' => break,
                3 | 4 => {
                    drop(guard);
                    eprintln!();
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "password input cancelled",
                    )
                    .into());
                }
                8 | 127 => {
                    while bytes.last().is_some_and(|b| b & 0xc0 == 0x80) {
                        bytes.pop();
                    }
                    bytes.pop();
                }
                byte => {
                    if bytes.len() >= 1024 {
                        return Err(Error::Config("password input is too long"));
                    }
                    bytes.push(byte);
                }
            }
        }
        Zeroizing::new(
            String::from_utf8(bytes.to_vec())
                .map_err(|_| Error::Config("password must be UTF-8"))?,
        )
    } else {
        let mut secret = Zeroizing::new(String::new());
        std::io::Read::take(std::io::stdin().lock(), 1026).read_line(&mut secret)?;
        secret
    };
    drop(guard);
    if terminal {
        eprintln!();
    }
    if secret.ends_with('\n') {
        secret.pop();
        if secret.ends_with('\r') {
            secret.pop();
        }
    }
    crate::users::validate_password(&secret)?;
    Ok(secret)
}
pub fn run(config: Config, args: &[String]) -> Result<()> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(Error::Config(USAGE));
    };
    if command == "list" {
        if args.len() != 1 {
            return Err(Error::Config(USAGE));
        }
        for user in Users::open(config.data_directory())?.list()? {
            println!("{}\t{}", user.username, user.id);
        }
        return Ok(());
    }
    let name = args.get(1).ok_or(Error::Config(USAGE))?;
    crate::users::canonical_username(name)?;
    let mut stdin = false;
    let mut adopt = false;
    for arg in &args[2..] {
        match arg.as_str() {
            "--password-stdin" if !stdin && matches!(command, "create" | "password") => {
                stdin = true
            }
            "--adopt-legacy" if !adopt && command == "create" => adopt = true,
            _ => return Err(Error::Config(USAGE)),
        }
    }
    let users = Users::open(config.data_directory())?;
    match command {
        "create" | "password" => {
            let terminal = std::io::stdin().is_terminal();
            if !terminal && !stdin {
                return Err(Error::Config(
                    "noninteractive password input requires --password-stdin",
                ));
            }
            let password = line("Password: ", terminal)?;
            if !stdin {
                let confirmation = line("Confirm password: ", terminal)?;
                if *password != *confirmation {
                    return Err(Error::Config("password confirmation does not match"));
                }
            }
            if command == "create" {
                let user = users.create(name, &password, adopt)?;
                println!("Created user {} ({})", user.username, user.id);
            } else {
                users.change_password(name, &password)?;
                println!("Password changed; existing sessions revoked");
            }
        }
        "disable" => {
            users.set_enabled(name, false)?;
            println!("User disabled; existing sessions revoked");
        }
        "enable" => {
            users.set_enabled(name, true)?;
            println!("User enabled");
        }
        _ => return Err(Error::Config(USAGE)),
    }
    Ok(())
}
