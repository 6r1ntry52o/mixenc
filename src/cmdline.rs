//! Windows command lines split into arguments by the Microsoft C runtime's rules (cl.exe's):
//! spaces and tabs separate arguments; `"` starts and ends a quoted part; `""` inside one is a
//! literal `"`; `2n` backslashes before a `"` are `n` backslashes and the `"` quotes; `2n+1` are `n`
//! backslashes and a literal `"`; other backslashes are literal. In a response file, line breaks
//! separate arguments too.

pub fn split(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut args = Vec::new();
    let mut i = 0;
    let blank = |c: char| matches!(c, ' ' | '\t' | '\r' | '\n');
    loop {
        while i < chars.len() && blank(chars[i]) {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let mut arg = String::new();
        let mut quoted = false;
        while i < chars.len() {
            let c = chars[i];
            if !quoted && blank(c) {
                break;
            }
            match c {
                '\\' => {
                    let start = i;
                    while i < chars.len() && chars[i] == '\\' {
                        i += 1;
                    }
                    let n = i - start;
                    if chars.get(i) == Some(&'"') {
                        arg.extend(std::iter::repeat_n('\\', n / 2));
                        if n % 2 == 1 {
                            arg.push('"');
                            i += 1;
                        }
                    } else {
                        arg.extend(std::iter::repeat_n('\\', n));
                    }
                }
                '"' => {
                    if quoted && chars.get(i + 1) == Some(&'"') {
                        arg.push('"');
                        i += 2;
                    } else {
                        quoted = !quoted;
                        i += 1;
                    }
                }
                _ => {
                    arg.push(c);
                    i += 1;
                }
            }
        }
        args.push(arg);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_cl() {
        assert_eq!(split(r#"/c /Iinclude main.cpp"#), ["/c", "/Iinclude", "main.cpp"]);
        assert_eq!(split(r#"/Fo"x64\Debug\\" a.cpp"#), [r"/Fox64\Debug\", "a.cpp"]);
        assert_eq!(split(r#"/D "A=\"x y\"" b"#), ["/D", r#"A="x y""#, "b"]);
        assert_eq!(split("/I\"C:\\Program Files\\x\"\r\n/DX"), [r"/IC:\Program Files\x", "/DX"]);
        assert_eq!(split(r#""a""b""#), [r#"a"b"#]);
    }
}
