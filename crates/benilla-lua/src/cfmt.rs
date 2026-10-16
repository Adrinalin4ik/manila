//! C `printf` conversions as glibc writes them, for `string.format`: exponents of two digits at
//! least, and `inf`/`nan` (`INF`/`NAN` for the upper-case conversions) for the non-finite values.

#[derive(Default, Clone, Copy)]
pub struct Spec {
    pub minus: bool,
    pub plus: bool,
    pub space: bool,
    pub alt: bool,
    pub zero: bool,
    pub width: usize,
    pub precision: Option<usize>,
}

impl Spec {
    /// No flag, width or precision: the conversion's own text.
    pub fn is_plain(&self) -> bool {
        !(self.minus || self.plus || self.space || self.alt || self.zero)
            && self.width == 0
            && self.precision.is_none()
    }
}

fn pad(body: String, prefix: &str, spec: &Spec, numeric: bool) -> String {
    let len = prefix.len() + body.len();
    if len >= spec.width {
        if prefix.is_empty() {
            return body;
        }
        return format!("{prefix}{body}");
    }
    let fill = spec.width - len;
    if spec.minus {
        format!("{prefix}{body}{}", " ".repeat(fill))
    } else if spec.zero && numeric {
        format!("{prefix}{}{body}", "0".repeat(fill))
    } else {
        format!("{}{prefix}{body}", " ".repeat(fill))
    }
}

fn sign(neg: bool, spec: &Spec) -> &'static str {
    if neg {
        "-"
    } else if spec.plus {
        "+"
    } else if spec.space {
        " "
    } else {
        ""
    }
}

/// `%d`/`%i` of a `long`.
pub fn fmt_int(v: i64, spec: &Spec) -> String {
    let mut digits = v.unsigned_abs().to_string();
    let zero_ok = spec.precision.is_none();
    if let Some(p) = spec.precision {
        if p == 0 && v == 0 {
            digits.clear();
        }
        while digits.len() < p {
            digits.insert(0, '0');
        }
    }
    let s = Spec {
        zero: spec.zero && zero_ok,
        ..*spec
    };
    pad(digits, sign(v < 0, spec), &s, true)
}

/// `%o`, `%u`, `%x`, `%X` of an `unsigned long`.
pub fn fmt_uint(v: u64, conv: u8, spec: &Spec) -> String {
    let mut digits = match conv {
        b'o' => format!("{v:o}"),
        b'x' => format!("{v:x}"),
        b'X' => format!("{v:X}"),
        _ => v.to_string(),
    };
    if let Some(p) = spec.precision {
        if p == 0 && v == 0 {
            digits.clear();
        }
        while digits.len() < p {
            digits.insert(0, '0');
        }
    }
    let prefix = match conv {
        b'o' if spec.alt && !digits.starts_with('0') => {
            digits.insert(0, '0');
            ""
        }
        b'x' if spec.alt && v != 0 => "0x",
        b'X' if spec.alt && v != 0 => "0X",
        _ => "",
    };
    let s = Spec {
        zero: spec.zero && spec.precision.is_none(),
        ..*spec
    };
    pad(digits, prefix, &s, true)
}

/// A mantissa and exponent from Rust's `{:e}`, the exponent as glibc writes it: a sign and two
/// digits at least.
fn exp_form(m: f64, prec: usize, upper: bool) -> String {
    let s = format!("{m:.prec$e}");
    let (mant, exp) = s.split_once('e').unwrap();
    let x: i32 = exp.parse().unwrap();
    let e = if upper { 'E' } else { 'e' };
    let sg = if x < 0 { '-' } else { '+' };
    format!("{mant}{e}{sg}{:02}", x.abs())
}

/// `format!("{a:.prec$}")` for a finite `a >= 0`, exactly, without float formatting: `a` is
/// `m * 2^e`, so `a * 10^prec` is `(m * 10^prec) >> -e`, rounded on the remainder as the
/// standard library rounds (ties to even). `None` outside the range this covers.
fn fixed_fast(a: f64, prec: usize) -> Option<String> {
    if prec > 9 || a >= 1e15 {
        return None;
    }
    let bits = a.to_bits();
    let biased = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    let (m, e) = if biased == 0 {
        (frac, -1074)
    } else {
        (frac | 1 << 52, biased - 1075)
    };
    let p10 = 10u128.pow(prec as u32);
    let n = m as u128 * p10;
    // Below 1e15, `e` is negative for every non-zero value.
    let q = if m == 0 {
        0
    } else {
        let sh = (-e) as u32;
        if sh >= 127 {
            // The value is under a half unit of the last place.
            0
        } else {
            let q = n >> sh;
            let r = n - (q << sh);
            let half = 1u128 << (sh - 1);
            q + (r > half || (r == half && q & 1 == 1)) as u128
        }
    };
    let (int, fr) = (q / p10, q % p10);
    Some(if prec == 0 {
        int.to_string()
    } else {
        format!("{int}.{fr:0prec$}")
    })
}

/// `%e`, `%E`, `%f`, `%g`, `%G` of a `double`.
pub fn fmt_float(v: f64, conv: u8, spec: &Spec) -> String {
    let neg = v.is_sign_negative() && !(v == 0.0 && !v.is_sign_negative());
    if !v.is_finite() {
        let upper = conv.is_ascii_uppercase();
        let body = match (v.is_nan(), upper) {
            (true, false) => "nan",
            (true, true) => "NAN",
            (false, false) => "inf",
            (false, true) => "INF",
        };
        let s = Spec {
            zero: false,
            ..*spec
        };
        return pad(body.to_string(), sign(neg, spec), &s, true);
    }
    let a = v.abs();
    let prec = spec.precision.unwrap_or(6);
    let mut body = match conv {
        b'f' => fixed_fast(a, prec).unwrap_or_else(|| format!("{a:.prec$}")),
        b'e' | b'E' => exp_form(a, prec, conv == b'E'),
        _ => {
            let p = prec.max(1);
            let x = if a == 0.0 {
                0
            } else {
                let s = format!("{a:.*e}", p - 1);
                s.split_once('e').unwrap().1.parse::<i32>().unwrap()
            };
            let mut s = if x < -4 || x >= p as i32 {
                exp_form(a, p - 1, conv == b'G')
            } else {
                format!("{a:.*}", (p as i32 - 1 - x).max(0) as usize)
            };
            if !spec.alt {
                // Trailing zeros of the fraction go, and a bare point with them.
                let (num, exp) = match s.find(['e', 'E']) {
                    Some(i) => (s[..i].to_string(), s[i..].to_string()),
                    None => (s.clone(), String::new()),
                };
                let num = if num.contains('.') {
                    num.trim_end_matches('0').trim_end_matches('.').to_string()
                } else {
                    num
                };
                s = num + &exp;
            }
            s
        }
    };
    // `#`: the point stays, for `%g` too (whose trailing zeros stay as well).
    if spec.alt && !body.contains('.') {
        match body.find(['e', 'E']) {
            Some(i) => body.insert(i, '.'),
            None => body.push('.'),
        }
    }
    pad(body, sign(neg, spec), spec, true)
}

/// `%s` with an optional precision.
pub fn fmt_str(s: &[u8], spec: &Spec) -> Vec<u8> {
    let s = match spec.precision {
        Some(p) => &s[..s.len().min(p)],
        None => s,
    };
    let fill = spec.width.saturating_sub(s.len());
    let mut out = Vec::with_capacity(s.len() + fill);
    if !spec.minus {
        out.extend(std::iter::repeat_n(b' ', fill));
    }
    out.extend_from_slice(s);
    if spec.minus {
        out.extend(std::iter::repeat_n(b' ', fill));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sp(f: &str) -> Spec {
        let mut s = Spec::default();
        let b = f.as_bytes();
        let mut i = 0;
        while i < b.len() && b"-+ #0".contains(&b[i]) {
            match b[i] {
                b'-' => s.minus = true,
                b'+' => s.plus = true,
                b' ' => s.space = true,
                b'#' => s.alt = true,
                _ => s.zero = true,
            }
            i += 1;
        }
        let mut w = 0;
        while i < b.len() && b[i].is_ascii_digit() {
            w = w * 10 + (b[i] - b'0') as usize;
            i += 1;
        }
        s.width = w;
        if i < b.len() && b[i] == b'.' {
            i += 1;
            let mut p = 0;
            while i < b.len() && b[i].is_ascii_digit() {
                p = p * 10 + (b[i] - b'0') as usize;
                i += 1;
            }
            s.precision = Some(p);
        }
        s
    }

    /// The fast `%f` agrees with the standard library's exact formatting everywhere it answers.
    #[test]
    fn fixed_fast_is_exact() {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let check = |a: f64, prec: usize| {
            if let Some(s) = fixed_fast(a, prec) {
                assert_eq!(s, format!("{a:.prec$}"), "{a:e} .{prec}");
            }
        };
        for _ in 0..1_000_000 {
            let r = next();
            let prec = (r % 10) as usize;
            // Values across magnitudes, and short decimals that land on exact ties.
            let a = match r % 4 {
                0 => f64::from_bits(next() >> 2) % 1e15,
                1 => (next() % 100_000_000) as f64 / 1000.0,
                2 => (next() % 1_000_000) as f64 / 8.0,
                _ => (next() % 10_000) as f64 / 3.0,
            };
            if a.is_finite() && a >= 0.0 {
                check(a, prec);
            }
        }
        for a in [
            0.0,
            0.5,
            1.5,
            2.5,
            0.125,
            0.375,
            1e-300,
            5e-324,
            999999999999999.9,
            0.05,
        ] {
            for prec in 0..=9 {
                check(a, prec);
            }
        }
    }

    #[test]
    fn printf_shapes() {
        assert_eq!(fmt_int(42, &sp("05")), "00042");
        assert_eq!(fmt_int(-42, &sp("5")), "  -42");
        assert_eq!(fmt_int(-42, &sp("-5")), "-42  ");
        assert_eq!(fmt_int(7, &sp("+.3")), "+007");
        assert_eq!(fmt_uint(255, b'x', &sp("#")), "0xff");
        assert_eq!(fmt_uint(255, b'X', &sp("04")), "00FF");
        assert_eq!(fmt_float(1.23456, b'f', &sp(".2")), "1.23");
        assert_eq!(fmt_float(1234.5, b'e', &sp("")), "1.234500e+03");
        assert_eq!(fmt_float(0.0001, b'g', &sp("")), "0.0001");
        assert_eq!(fmt_float(1e-5, b'g', &sp("")), "1e-05");
        assert_eq!(fmt_float(100000.0, b'g', &sp("")), "100000");
        assert_eq!(fmt_float(1e6, b'g', &sp("")), "1e+06");
        assert_eq!(fmt_float(f64::INFINITY, b'f', &sp("5")), "  inf");
        assert_eq!(fmt_float(f64::NEG_INFINITY, b'E', &sp("")), "-INF");
        assert_eq!(fmt_float(2.5, b'f', &sp("08.3")), "0002.500");
        assert_eq!(fmt_float(123456.0, b'g', &sp("#")), "123456.");
        assert_eq!(fmt_float(1.0, b'g', &sp("#.0")), "1.");
        assert_eq!(fmt_float(1e20, b'G', &sp("#.0")), "1.E+20");
        assert_eq!(fmt_str(b"abc", &sp("5")), b"  abc");
        assert_eq!(fmt_str(b"abcdef", &sp(".2")), b"ab");
    }
}
