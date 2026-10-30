// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let (na, nb) = (b, a % b);
        a = na;
        b = nb;
    }
    a
}

fn modpow(mut n: u128, mut e: u128, m: u128) -> u128 {
    if m == 1 {
        return 0;
    }

    let mut result = 1;
    n %= m;
    while e > 0 {
        if e % 2 == 1 {
            result = (result * n) % m;
        }
        e >>= 1;
        n = (n * n) % m;
    }
    result
}

/// Factorize the given number into its two prime factors.
///
/// The algorithm here is a faster variant of [Pollard's rho algorithm],
/// published by [Richard Brent], based on
/// <https://comeoncodeon.wordpress.com/2010/09/18/pollard-rho-brent-integer-factorization/>.
///
/// Pollard's rho algorithm: <https://en.wikipedia.org/wiki/Pollard%27s_rho_algorithm>
/// Richard Brent: <https://maths-people.anu.edu.au/~brent/pd/rpb051i.pdf>
///
/// # Panics
/// Panics if the input is not a semiprime or exceeds the bounded work budget.
/// Use [`try_factorize`] for untrusted inputs.
pub fn factorize(pq: u64) -> (u64, u64) {
    try_factorize(pq).expect("invalid pq or factorization work limit exceeded")
}

/// Factor a semiprime with bounded work, returning `None` for invalid inputs
/// or when the work budget is exhausted. Suitable for untrusted handshake data.
/// Unlike [`factorize`], this function never panics on an invalid number.
#[allow(clippy::many_single_char_names)]
pub fn try_factorize(pq: u64) -> Option<(u64, u64)> {
    // Small divisors also cover tiny composites for which the rho seeds degenerate.
    for p in [
        2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71, 73, 79, 83, 89,
        97,
    ] {
        if pq >= p * p && pq.is_multiple_of(p) {
            let q = pq / p;
            return is_prime(q).then_some((p, q));
        }
    }
    if pq < 4 || is_prime(pq) {
        return None;
    }
    let root = pq.isqrt();
    if root * root == pq {
        return is_prime(root).then_some((root, root));
    }
    let mut work = 1_250_000;
    const ATTEMPTS: [u64; 5] = [43, 47, 53, 59, 61];
    for attempt in ATTEMPTS {
        // > Note that this algorithm may not find the factors and will return failure for composite n.
        // > In that case, use a different f(x) and try again [...] We choose f(x) = x*x + c
        // Thus by choosing a different `c` we're changing `f(x)` and can try again.
        // Prime factors are used for the attempts in the hopes they'll be more likely to work.
        let c = (attempt * (pq / 103)).max(1);
        // Each restart, including the fallback loop, shares this finite budget.
        if let Some((p, q)) = factorize_with_param(pq, c, &mut work)
            && p > 1
            && q > 1
            && p.checked_mul(q) == Some(pq)
        {
            return (is_prime(p) && is_prime(q)).then_some((p, q));
        }
    }
    None
}

// Deterministic Miller-Rabin bases covering all u64 values. Arithmetic is
// widened before multiplication, including inputs close to u64::MAX.
fn is_prime(n: u64) -> bool {
    if n < 2 || n.is_multiple_of(2) {
        return n == 2;
    }
    let s = (n - 1).trailing_zeros();
    let d = (n - 1) >> s;
    for a in [2, 325, 9375, 28178, 450775, 9780504, 1795265022] {
        if a % n == 0 {
            continue;
        }
        let mut x = modpow(a as u128, d as u128, n as u128);
        if x == 1 || x == u128::from(n - 1) {
            continue;
        }
        let mut witness = true;
        for _ in 1..s {
            x = x * x % u128::from(n);
            if x == u128::from(n - 1) {
                witness = false;
                break;
            }
        }
        if witness {
            return false;
        }
    }
    true
}

fn factorize_with_param(pq: u64, c: u64, work: &mut usize) -> Option<(u64, u64)> {
    if pq.is_multiple_of(2) {
        return Some((2, pq / 2));
    }

    let pq = pq as u128;
    fn abs_sub(a: u128, b: u128) -> u128 {
        a.max(b) - a.min(b)
    }

    // Random values in the range of 1..pq, chosen by fair dice roll.
    // c is an input free to change in case the chosen value fails.
    let mut y = 3 * (pq / 7);
    let c = c as u128;
    let m = 128;
    let mut g = 1u128;
    let mut r = 1u128;
    let mut q = 1u128;
    let mut x = 0u128;
    let mut ys = 0u128;

    while g == 1 {
        x = y;
        for _ in 0..r {
            *work = work.checked_sub(1)?;
            y = (modpow(y, 2, pq) + c) % pq;
        }

        let mut k = 0;
        while k < r && g == 1 {
            ys = y;
            for _ in 0..m.min(r - k) {
                *work = work.checked_sub(1)?;
                y = (modpow(y, 2, pq) + c) % pq;
                q = (q * abs_sub(x, y)) % pq;
            }

            g = gcd(q, pq);
            k += m;
        }

        r *= 2;
    }

    if g == pq {
        loop {
            *work = work.checked_sub(1)?;
            ys = (modpow(ys, 2, pq) + c) % pq;
            g = gcd(abs_sub(x, ys), pq);
            if g > 1 {
                break;
            }
        }
    }

    let (p, q) = (g as u64, (pq / g) as u64);
    Some((p.min(q), p.max(q)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_factorization_small_and_invalid() {
        for n in 0..256 {
            let expected = (2..=n / 2)
                .find_map(|p| (n % p == 0 && is_prime(p) && is_prime(n / p)).then_some((p, n / p)));
            assert_eq!(try_factorize(n), expected, "{n}");
        }
        assert_eq!(try_factorize(u64::MAX), None);
        assert_eq!(try_factorize(18_446_744_073_709_551_557), None);
        assert_eq!(factorize_with_param(1470626929934143021, 43, &mut 0), None);
    }

    #[test]
    fn factorization_has_a_process_deadline() {
        use std::{
            process::Command,
            time::{Duration, Instant},
        };
        const CHILD: &str = "GRAMMERS_FACTORIZE_DEADLINE_CHILD";
        if std::env::var_os(CHILD).is_some() {
            checked_factorization_small_and_invalid();
            test_factorization_1();
            test_factorization_2();
            test_factorization_3();
            factorization_u64_edges_and_shared_budget();
            println!("factorization-deadline-child-completed");
            return;
        }
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "--nocapture",
                "factorize::tests::factorization_has_a_process_deadline",
            ])
            .stdout(std::process::Stdio::piped())
            .env(CHILD, "1")
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success());
                let output = child.wait_with_output().unwrap();
                assert!(
                    String::from_utf8(output.stdout)
                        .unwrap()
                        .contains("factorization-deadline-child-completed")
                );
                break;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                panic!("factorization exceeded process deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn factorization_u64_edges_and_shared_budget() {
        let p = 4_294_967_291u64;
        let q = 4_294_967_279u64;
        assert_eq!(try_factorize(p * p), Some((p, p)));
        assert_eq!(try_factorize(p * q), Some((q, p)));
        assert_eq!(try_factorize(101 * 103 * 107), None);
        let mut work = 100;
        assert_eq!(factorize_with_param(p * q, 43, &mut work), None);
        assert_eq!(work, 0);
        assert_eq!(factorize_with_param(p * q, 47, &mut work), None);
    }

    #[test]
    fn test_factorization_1() {
        let pq = factorize(1470626929934143021);
        assert_eq!(pq, (1206429347, 1218991343));
    }

    #[test]
    fn test_factorization_2() {
        let pq = factorize(2363612107535801713);
        assert_eq!(pq, (1518968219, 1556064227));
    }

    #[test]
    fn test_factorization_3() {
        let pq = factorize(2804275833720261793);
        assert_eq!(pq, (1555252417, 1803100129));
    }
}
