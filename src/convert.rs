const EN: &str = "`1234567890-=qwertyuiop[]\\asdfghjkl;'zxcvbnm,./~!@#$%^&*()_+QWERTYUIOP{}|ASDFGHJKL:\"ZXCVBNM<>?";
const TH: &str = "_ๅ/-ภถุึคตจขชๆไำพะัีรนยบลฃฟหกดเ้่าสวงผปแอิืทมใฝ%+๑๒๓๔ู฿๕๖๗๘๙๐\"ฎฑธํ๊ณฯญฐ,ฅฤฆฏโฌ็๋ษศซ.()ฉฮฺ์?ฒฬฦ";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Target {
    Thai,
    English,
}

pub fn convert(text: &str) -> Option<(String, Target)> {
    let thai = text.chars().any(|c| ('\u{0e00}'..='\u{0e7f}').contains(&c));
    let latin = text.chars().any(|c| c.is_ascii_alphabetic());
    if thai == latin {
        return None;
    }
    let (from, to, target) = if thai {
        (TH, EN, Target::English)
    } else {
        (EN, TH, Target::Thai)
    };
    // shortcut: Kedmanee and US layouts only; add a layout table if another layout is requested.
    let converted: String = text
        .chars()
        .map(|c| {
            from.chars()
                .position(|v| v == c)
                .and_then(|i| to.chars().nth(i))
                .unwrap_or(c)
        })
        .collect();
    (converted != text).then_some((converted, target))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tables_and_round_trip() {
        assert_eq!(EN.chars().count(), TH.chars().count());
        let mut unique = std::collections::HashSet::new();
        assert!(TH.chars().all(|c| unique.insert(c)));
        for (en, th) in EN.chars().zip(TH.chars()) {
            assert_eq!(
                TH.chars()
                    .position(|c| c == th)
                    .map(|i| EN.chars().nth(i).unwrap()),
                Some(en)
            );
        }
    }
    #[test]
    fn requested_example_both_directions() {
        assert_eq!(convert("c]h;"), Some(("แล้ว".into(), Target::Thai)));
        assert_eq!(convert("แล้ว"), Some(("c]h;".into(), Target::English)));
        assert_eq!(convert("hello"), Some(("้ำสสน".into(), Target::Thai)));
    }
    #[test]
    fn leading_thai_marks_round_trip_without_reordering_or_removal() {
        for (english, thai) in [("n", "ื"), ("b", "ิ"), ("notify", "ืนะรดั"), ("hello", "้ำสสน")]
        {
            assert_eq!(convert(english), Some((thai.into(), Target::Thai)));
            assert_eq!(convert(thai), Some((english.into(), Target::English)));
            assert_eq!(
                convert(&format!(" {thai} ")),
                Some((format!(" {english} "), Target::English))
            );
        }
    }
    #[test]
    fn ambiguous_text_is_untouched() {
        for text in ["", "123", "ไทยabc", "😀", " \t\n"] {
            assert_eq!(convert(text), None);
        }
    }
}
