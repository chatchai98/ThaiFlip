use crate::convert::Target;
use windows::{
    core::{w, PCWSTR},
    Win32::{Globalization::*, System::Com::*},
};

// shortcut: common words supplement missing Windows language packs; install language dictionaries for broader coverage.
const TH: &str = "แล้ว สวัสดี ครับ ค่ะ คะ ขอบคุณ ขอโทษ ใช่ ไม่ ได้ เป็น มี ที่ และ ของ การ ให้ ไป มา เรา คุณ ผม ฉัน เขา เธอ มัน ใน กับ จะ นี้ นั้น ทุก คน ทำ งาน วันนี้ พรุ่งนี้ เมื่อวาน รัก มาก ดี อยาก เรียน ภาษา ไทย อังกฤษ อะไร ทำไม อย่างไร เท่าไหร่ อยู่ ไหน หรือ เพราะ แต่ ถ้า ก็ ยัง ต้อง อีก ด้วย นะ คอมพิวเตอร์ ทดสอบ";
const EN: &str = "hello world test rust windows english thai the a an and or is are was were be been to from with for of in on at it this that these those i you he she we they my your our their have has had do does did can could would should will not yes no please thank thanks sorry good bad morning night today tomorrow yesterday work home name time people know think want need like love use make get go come see all any some one two three file app icon popup cursor tab code password";

fn lexical(word: &str, language: Target) -> Option<&str> {
    let word = word.trim();
    let word = word.trim_matches(|c: char| c.is_ascii_punctuation() && c != '\'' && c != '-');
    let valid = match language {
        Target::English => {
            word.chars().any(|c| c.is_ascii_alphabetic())
                && word
                    .chars()
                    .all(|c| c.is_ascii_alphabetic() || c == '\'' || c == '-')
        }
        Target::Thai => {
            word.chars()
                .next()
                .is_some_and(|c| ('ก'..='ฮ').contains(&c) || ('เ'..='ไ').contains(&c))
                && word.chars().all(|c| ('ก'..='๎').contains(&c))
        }
    };
    valid.then_some(word)
}

fn common(word: &str, language: Target) -> bool {
    match language {
        Target::Thai => TH.split_whitespace().any(|v| v == word),
        Target::English => EN.split_whitespace().any(|v| v.eq_ignore_ascii_case(word)),
    }
}

pub struct Detector {
    thai: Option<ISpellChecker>,
    english: Option<ISpellChecker>,
}

impl Detector {
    pub unsafe fn replacement(&self, text: &str, automatic: bool) -> Option<(String, Target)> {
        let result = crate::convert::convert(text)?;
        if automatic {
            let source = if result.1 == Target::Thai {
                Target::English
            } else {
                Target::Thai
            };
            if self.known(text.trim_end_matches(' '), source) != Some(false)
                || self.known(result.0.trim_end_matches(' '), result.1) != Some(true)
            {
                return None;
            }
        }
        Some(result)
    }
    pub unsafe fn new() -> Self {
        let factory: Option<ISpellCheckerFactory> =
            CoCreateInstance(&SpellCheckerFactory, None, CLSCTX_INPROC_SERVER).ok();
        Self {
            thai: factory
                .as_ref()
                .and_then(|f| f.CreateSpellChecker(w!("th-TH")).ok()),
            english: factory
                .as_ref()
                .and_then(|f| f.CreateSpellChecker(w!("en-US")).ok()),
        }
    }

    unsafe fn known(&self, word: &str, language: Target) -> Option<bool> {
        let Some(word) = lexical(word, language) else {
            return Some(false);
        };
        if common(word, language) {
            return Some(true);
        }
        let checker = match language {
            Target::Thai => &self.thai,
            Target::English => &self.english,
        }
        .as_ref()?;
        let wide: Vec<u16> = word.encode_utf16().chain(Some(0)).collect();
        let errors = checker.Check(PCWSTR(wide.as_ptr())).ok()?;
        let mut error = None;
        errors.Next(&mut error).ok().ok()?;
        Some(error.is_none())
    }

    pub unsafe fn suspicious(&self, word: &str, replacement: &str, target: Target) -> bool {
        suspicious(word, replacement, target, |word, lang| {
            self.known(word, lang)
        })
    }
}

fn suspicious(
    word: &str,
    replacement: &str,
    target: Target,
    known: impl Fn(&str, Target) -> Option<bool>,
) -> bool {
    let source = match target {
        Target::Thai => Target::English,
        Target::English => Target::Thai,
    };
    known(word, source) != Some(true) && known(replacement, target) == Some(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::convert;
    #[test]
    fn manual_is_not_dictionary_gated_and_auto_keeps_the_delimiter() {
        let detector = Detector {
            thai: None,
            english: None,
        };
        unsafe {
            assert!(detector.replacement("hello", false).is_some());
            assert!(detector.replacement("unknownname", false).is_some());
            assert!(detector.replacement("hello ", true).is_none());
            assert_eq!(
                detector.replacement("c]h; ", true),
                Some(("แล้ว ".into(), Target::Thai))
            );
            assert!(detector.replacement("dki ", true).is_none());
            assert_eq!(
                detector
                    .replacement(&"a".repeat(512), false)
                    .unwrap()
                    .0
                    .chars()
                    .count(),
                512
            );
        }
    }
    fn basic(word: &str, language: Target) -> Option<bool> {
        let Some(word) = lexical(word, language) else {
            return Some(false);
        };
        common(word, language).then_some(true)
    }
    #[test]
    fn warns_for_known_corrections_without_flagging_normal_words() {
        for (word, expected) in [
            ("c]h;", true),
            ("้ำสสน", true),
            ("hello", false),
            ("แล้ว", false),
            ("unknownname", false),
        ] {
            let (replacement, target) = convert(word).unwrap();
            assert_eq!(
                suspicious(word, &replacement, target, basic),
                expected,
                "{word}"
            );
        }
    }
    #[test]
    fn dictionary_failure_does_not_claim_a_typo() {
        assert!(!suspicious("something", "ทดสอบ", Target::Thai, |_, _| None));
        assert_eq!(lexical("c]h;", Target::English), None);
        assert_eq!(lexical("้ำสสน", Target::Thai), None);
        assert_eq!(lexical("hello!", Target::English), Some("hello"));
    }
}
