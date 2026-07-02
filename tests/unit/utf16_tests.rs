use super::Utf16Index;

// Telegram MessageEntity offsets/lengths are UTF-16 code units. BMP CJK chars
// count 1 unit (3 UTF-8 bytes); supplementary-plane emoji count 2 units
// (surrogate pair, 4 bytes).

#[test]
fn ascii_command_slice() {
    let text = "/id@bot 你好🤝";
    let idx = Utf16Index::new(text);
    assert_eq!(&text[idx.byte_range(0, 7).unwrap()], "/id@bot");
}

#[test]
fn cjk_and_emoji_slice() {
    let text = "/id@bot 你好🤝";
    let idx = Utf16Index::new(text);
    assert_eq!(&text[idx.byte_range(8, 4).unwrap()], "你好🤝");
}

#[test]
fn offset_splitting_surrogate_pair_is_none() {
    let text = "/id@bot 你好🤝";
    let idx = Utf16Index::new(text);
    assert_eq!(idx.byte_range(11, 1), None); // would split 🤝
}

#[test]
fn out_of_bounds_is_none() {
    let text = "/id@bot 你好🤝"; // 12 UTF-16 units total
    let idx = Utf16Index::new(text);
    assert_eq!(idx.byte_range(0, 13), None);
    assert_eq!(idx.byte_range(99, 1), None);
}

#[test]
fn zero_length_at_exact_end_is_empty() {
    let text = "/id@bot 你好🤝";
    let idx = Utf16Index::new(text);
    assert_eq!(&text[idx.byte_range(12, 0).unwrap()], "");
}

#[test]
fn command_after_cjk_with_args_remainder() {
    let text = "你好 /start@my_bot arg";
    let idx = Utf16Index::new(text);
    let r = idx.byte_range(3, 13).unwrap();
    assert_eq!(&text[r.clone()], "/start@my_bot");
    assert_eq!(text[r.end..].trim_start(), "arg");
}

#[test]
fn mention_after_two_surrogate_pairs() {
    let text = "🤝🤝 @alice ok";
    let idx = Utf16Index::new(text);
    assert_eq!(&text[idx.byte_range(5, 6).unwrap()], "@alice");
}

#[test]
fn command_after_zwj_family_sequence() {
    let text = "hi 👨\u{200D}👩\u{200D}👧 /cmd"; // family = 8 UTF-16 units
    let idx = Utf16Index::new(text);
    assert_eq!(&text[idx.byte_range(12, 4).unwrap()], "/cmd");
}
