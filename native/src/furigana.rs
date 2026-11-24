use lindera::dictionary::load_dictionary;
use lindera::mode::Mode;
use lindera::segmenter::Segmenter;
use serde::Serialize;
use std::borrow::Cow;
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FuriganaToken {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reading: Option<String>,
}

static SEGMENTER: OnceLock<Option<Segmenter>> = OnceLock::new();

fn segmenter() -> Option<&'static Segmenter> {
    SEGMENTER
        .get_or_init(|| {
            let dictionary = load_dictionary("embedded://ipadic").ok()?;
            Some(Segmenter::new(Mode::Normal, dictionary, None))
        })
        .as_ref()
}

fn is_kanji(c: char) -> bool {
    let code = c as u32;
    matches!(
        code,
        0x3005 | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2A6DF
    )
}

fn to_hiragana_char(c: char) -> char {
    let code = c as u32;
    if (0x30A1..=0x30F6).contains(&code) {
        char::from_u32(code - 0x60).unwrap_or(c)
    } else {
        c
    }
}

fn katakana_to_hiragana(s: &str) -> String {
    s.chars().map(to_hiragana_char).collect()
}

fn push_token(out: &mut Vec<FuriganaToken>, text: String, reading: Option<String>) {
    if text.is_empty() {
        return;
    }
    if reading.is_none()
        && let Some(last) = out.last_mut()
        && last.reading.is_none()
    {
        last.text.push_str(&text);
        return;
    }
    out.push(FuriganaToken { text, reading });
}

fn align_middle(surf: &[char], reading: &[char], out: &mut Vec<FuriganaToken>) {
    if surf.is_empty() {
        return;
    }
    if !surf.iter().any(|&c| is_kanji(c)) || reading.is_empty() {
        push_token(out, surf.iter().collect(), None);
        return;
    }

    // Look for a middle kana span between kanji (e.g. 飲み物 -> のみもの)
    let mut idx = 0;
    while idx < surf.len() && is_kanji(surf[idx]) {
        idx += 1;
    }
    if idx > 0 && idx < surf.len() {
        let mut kana_end = idx;
        while kana_end < surf.len() && !is_kanji(surf[kana_end]) {
            kana_end += 1;
        }
        if kana_end < surf.len() {
            let needle: Vec<char> = surf[idx..kana_end]
                .iter()
                .copied()
                .map(to_hiragana_char)
                .collect();
            let min_pos = 1;
            let max_pos = reading.len().saturating_sub(needle.len() + 1);
            if min_pos <= max_pos {
                let mut matches = Vec::new();
                for pos in min_pos..=max_pos {
                    if reading[pos..pos + needle.len()] == needle[..] {
                        matches.push(pos);
                    }
                }
                if matches.len() == 1 {
                    let pos = matches[0];
                    align_middle(&surf[..idx], &reading[..pos], out);
                    push_token(out, surf[idx..kana_end].iter().collect(), None);
                    align_middle(&surf[kana_end..], &reading[pos + needle.len()..], out);
                    return;
                }
            }
        }
    }

    let text: String = surf.iter().collect();
    let hira: String = reading.iter().collect();
    let reading_opt = if hira.is_empty() || katakana_to_hiragana(&text) == hira {
        None
    } else {
        Some(hira)
    };
    push_token(out, text, reading_opt);
}

fn align_surface_reading(surface: &str, reading_kana: &str, out: &mut Vec<FuriganaToken>) {
    let surf: Vec<char> = surface.chars().collect();
    let reading: Vec<char> = katakana_to_hiragana(reading_kana).chars().collect();

    if !surf.iter().any(|&c| is_kanji(c)) || reading.is_empty() {
        push_token(out, surface.to_string(), None);
        return;
    }

    let mut s_start = 0;
    let mut r_start = 0;
    while s_start < surf.len()
        && r_start < reading.len()
        && !is_kanji(surf[s_start])
        && to_hiragana_char(surf[s_start]) == reading[r_start]
    {
        s_start += 1;
        r_start += 1;
    }

    let mut s_end = surf.len();
    let mut r_end = reading.len();
    while s_end > s_start
        && r_end > r_start
        && !is_kanji(surf[s_end - 1])
        && to_hiragana_char(surf[s_end - 1]) == reading[r_end - 1]
    {
        s_end -= 1;
        r_end -= 1;
    }

    if s_start > 0 {
        push_token(out, surf[..s_start].iter().collect(), None);
    }
    align_middle(&surf[s_start..s_end], &reading[r_start..r_end], out);
    if s_end < surf.len() {
        push_token(out, surf[s_end..].iter().collect(), None);
    }
}

pub fn annotate_furigana(text: &str) -> Vec<FuriganaToken> {
    if text.is_empty() || !text.chars().any(is_kanji) {
        return Vec::new();
    }

    let Some(seg) = segmenter() else {
        return Vec::new();
    };

    let Ok(mut tokens) = seg.segment(Cow::Borrowed(text)) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for token in tokens.iter_mut() {
        let surface = token.surface.as_ref().to_string();
        let reading = token
            .details()
            .get(7)
            .copied()
            .filter(|r| !r.is_empty() && *r != "*")
            .unwrap_or("");
        align_surface_reading(&surface, reading, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_non_kanji_text() {
        assert!(annotate_furigana("hello world").is_empty());
        assert!(annotate_furigana("ひらがな カタカナ！").is_empty());
    }

    #[test]
    fn annotates_kanji_and_strips_okurigana() {
        let tokens = annotate_furigana("楽しいお帰り");
        let joined: String = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(joined, "楽しいお帰り");
        assert!(tokens.contains(&FuriganaToken {
            text: "楽".to_string(),
            reading: Some("たの".to_string()),
        }));
        assert!(tokens.contains(&FuriganaToken {
            text: "帰".to_string(),
            reading: Some("かえ".to_string()),
        }));
    }

    #[test]
    fn splits_internal_okurigana_in_compound_words() {
        let tokens = annotate_furigana("飲み物");
        assert_eq!(
            tokens,
            vec![
                FuriganaToken {
                    text: "飲".to_string(),
                    reading: Some("の".to_string()),
                },
                FuriganaToken {
                    text: "み".to_string(),
                    reading: None,
                },
                FuriganaToken {
                    text: "物".to_string(),
                    reading: Some("もの".to_string()),
                },
            ]
        );
    }

    #[test]
    fn preserves_full_sentence_and_merges_plain_runs() {
        let input = "今日めっちゃ楽しいんだけど！";
        let tokens = annotate_furigana(input);
        let joined: String = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(joined, input);
        assert_eq!(
            tokens[0],
            FuriganaToken {
                text: "今日".to_string(),
                reading: Some("きょう".to_string()),
            }
        );
        assert_eq!(
            tokens[1],
            FuriganaToken {
                text: "めっちゃ".to_string(),
                reading: None,
            }
        );
    }
}
