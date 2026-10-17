//! Passwords are read from stdin/terminal, never process arguments.
use crate::{
    Config, Error, Result,
    users::{Role, Users},
};
use std::io::{BufRead, IsTerminal, Write};
use zeroize::Zeroizing;
const USAGE: &str = "usage: syncserver users create NAME [--password-stdin] [--adopt-legacy] [--admin] | list | password NAME [--password-stdin] | disable NAME | enable NAME | role NAME admin|user | quota NAME BYTES|default";
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
/// Decimal byte sizes with an optional `K`/`M`/`G`/`T` suffix, matching how the
/// quota contract counts logical bytes.
fn quota_bytes(raw: &str) -> Result<u64> {
    let digits = raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len());
    let (number, suffix) = raw.split_at(digits);
    let scale = match suffix.trim().to_ascii_uppercase().as_str() {
        "" => 1u64,
        "K" => 1_000,
        "M" => 1_000_000,
        "G" => 1_000_000_000,
        "T" => 1_000_000_000_000,
        _ => {
            return Err(Error::Config(
                "quota must be BYTES with an optional K/M/G/T suffix",
            ));
        }
    };
    let value: u64 = number
        .parse()
        .map_err(|_| Error::Config("quota must be a positive integer"))?;
    if value == 0 {
        return Err(Error::Config("quota must be a positive integer"));
    }
    value
        .checked_mul(scale)
        .ok_or(Error::Config("quota is too large"))
}
pub fn run(config: Config, args: &[String]) -> Result<()> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err(Error::Config(USAGE));
    };
    if command == "list" {
        if args.len() != 1 {
            return Err(Error::Config(USAGE));
        }
        for account in Users::open(config.data_directory())?.accounts()? {
            println!(
                "{}\t{}\t{}\t{}\t{}",
                account.username,
                account.id,
                account.role.as_str(),
                if account.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                account
                    .quota_bytes
                    .map_or("default".to_owned(), |q| q.to_string()),
            );
        }
        return Ok(());
    }
    let name = args.get(1).ok_or(Error::Config(USAGE))?;
    crate::users::canonical_username(name)?;
    // `role` and `quota` take a plain value in third position; everything else only
    // accepts flags, so a mistyped argument is refused rather than ignored.
    let takes_value = matches!(command, "role" | "quota");
    if takes_value && args.len() != 3 {
        return Err(Error::Config(USAGE));
    }
    let mut stdin = false;
    let mut adopt = false;
    let mut admin = false;
    for arg in args.iter().skip(2).filter(|_| !takes_value) {
        match arg.as_str() {
            "--password-stdin" if !stdin && matches!(command, "create" | "password") => {
                stdin = true
            }
            "--adopt-legacy" if !adopt && command == "create" => adopt = true,
            "--admin" if !admin && command == "create" => admin = true,
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
                let role = if admin { Role::Admin } else { Role::User };
                let user = users.create(name, &password, adopt, role)?;
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
        "role" => {
            let role = Role::parse(&args[2])?;
            users.set_role(&users.id_of(name)?, role)?;
            println!("User role set to {}", role.as_str());
        }
        "quota" => {
            let bytes = if args[2] == "default" {
                None
            } else {
                Some(quota_bytes(&args[2])?)
            };
            users.set_quota(&users.id_of(name)?, bytes)?;
            println!(
                "User quota set to {}",
                bytes.map_or_else(
                    || "the deployment default".to_owned(),
                    |value| value.to_string()
                )
            );
        }
        _ => return Err(Error::Config(USAGE)),
    }
    Ok(())
}
