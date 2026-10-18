/** Typing a query should not run one request per keystroke. */
import { useEffect, useState } from 'react'

/**
 * `value`, held back until it has stopped changing for `delay` milliseconds.
 *
 * A search box drives a server-side query, so every keystroke would otherwise be a
 * round trip and a full redraw of the results behind it.
 */
export function useDebounced<T>(value: T, delay = 250): T {
  const [settled, setSettled] = useState(value)
  useEffect(() => {
    const timer = window.setTimeout(() => setSettled(value), delay)
    return () => window.clearTimeout(timer)
  }, [value, delay])
  return settled
}
