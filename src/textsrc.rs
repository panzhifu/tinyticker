//! 外部文本源：把另一个进程写的文本文件显示在挂件的状态行上。
//!
//! 借鉴 Catime 的 `output.txt` 机制（只借设计，不搬代码），四条规则：
//! 1. 每帧只 `stat`，靠 `(mtime, len)` 判断有没有变，没变就不读文件；
//! 2. 有硬大小上限，超了直接不读——这个文件由别的进程写，不能信它有多大；
//! 3. 文件缺失、为空、读坏了一律清空，绝不留上一次的内容在屏上骗人；
//! 4. 没配 `text_source` 就完全不碰文件系统。
//!
//! 只做只读显示：不执行任何外部程序，所以「插件」这个词在这里比它听起来保守。

use crate::config::expand_tilde;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// 愿意读的上限；超过就当作不可信来源直接跳过。显示只需要一行。
pub const MAX_BYTES: u64 = 64 * 1024;

/// 一个外部文本文件。`path` 为 `None` 时整个模块不产生任何 IO。
pub struct Source {
    path: Option<PathBuf>,
    /// 上次看到的 (大小, 修改时间)；用来跳过没变的轮询
    stamp: Option<(u64, SystemTime)>,
    text: Option<String>,
}

impl Source {
    /// `raw` 是配置里的原值；空串与纯空白按未配置处理。
    pub fn new(raw: Option<&str>) -> Self {
        Self { path: raw.map(expand_tilde).filter(|p| !p.as_os_str().is_empty()), stamp: None, text: None }
    }

    /// 当前要显示的文本；`None` = 状态行回到挂件自己的内容。
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// 每帧调一次。绝大多数情况下只是一次 `stat`，内容真变了才读文件。
    pub fn refresh(&mut self) {
        let Some(path) = &self.path else { return };
        match stat(path) {
            Ok(stamp) if Some(stamp) == self.stamp => {}
            Ok(stamp) => {
                self.stamp = Some(stamp);
                self.text = read_and_extract(path, stamp.0);
            }
            // 文件没了 / stat 失败：清空，让状态行回到正常内容
            Err(_) => {
                self.stamp = None;
                self.text = None;
            }
        }
    }
}

fn stat(path: &Path) -> io::Result<(u64, SystemTime)> {
    let meta = fs::metadata(path)?;
    let modified = meta.modified()?;
    Ok((meta.len(), modified))
}

/// 读文件并取出一行可显示文本。任何一步不合格都返回 `None`。
fn read_and_extract(path: &Path, len: u64) -> Option<String> {
    if len > MAX_BYTES {
        eprintln!("⚠️ text_source 超过 {MAX_BYTES} 字节，跳过：{}", path.display());
        return None;
    }
    let bytes = fs::read(path).ok()?;
    let text = extract(&bytes);
    (!text.is_empty()).then(|| text.to_string())
}

/// 从原始字节里取出第一行非空文本。
///
/// 只取第一行、去掉行尾 `\r`、丢掉控制字符；不是合法 UTF-8 就当没写完，
/// 因为撕裂的写入正是半截多字节序列的典型来源。
fn extract(bytes: &[u8]) -> &str {
    match std::str::from_utf8(bytes) {
        Ok(text) => first_line(text),
        Err(_) => "",
    }
}

fn first_line(text: &str) -> &str {
    text.lines()
        .map(|l| l.trim_matches(|c: char| c == '\r' || c == ' ' || c == '\t'))
        .find(|l| !l.is_empty() && l.chars().all(|c| !c.is_control()))
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_the_first_non_empty_line() {
        assert_eq!(first_line("hello\nworld\n"), "hello");
        assert_eq!(first_line("\n  \nsecond\nthird"), "second");
        assert_eq!(first_line("only"), "only");
    }

    #[test]
    fn strips_crlf_and_surrounding_blanks() {
        assert_eq!(first_line("  build 42 \r\n"), "build 42");
        assert_eq!(first_line("\t42%\t\n"), "42%");
    }

    #[test]
    fn empty_and_blank_only_input_yield_nothing() {
        assert_eq!(first_line(""), "");
        assert_eq!(first_line("\n\n   \n"), "");
    }

    #[test]
    fn control_characters_are_rejected() {
        // 终端转义序列会把状态行变成注入通道，整行丢掉而不是画出来
        assert_eq!(first_line("\x1b[2J\r"), "");
        assert_eq!(first_line("ok\x00tail\nnext\n"), "next");
    }

    #[test]
    fn invalid_utf8_is_treated_as_no_content() {
        // 撕裂写入的典型形态：半截多字节序列
        assert_eq!(extract(&[0xe4, 0xb8, 0x61]), "");
        assert_eq!(extract(b"plain ascii\n"), "plain ascii");
    }

    #[test]
    fn unconfigured_source_does_nothing() {
        let mut s = Source::new(None);
        s.refresh();
        assert_eq!(s.text(), None);
        // 配置成空串/纯空白也等于没配
        assert!(Source::new(Some("")).path.is_none());
        assert!(Source::new(Some("   ")).path.is_none());
        assert!(Source::new(None).path.is_none());
    }

    #[test]
    fn reads_a_real_file_and_clears_when_it_vanishes() {
        let dir = std::env::temp_dir().join("tinyticker-textsrc-test");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.txt");
        let raw = path.to_str().unwrap().to_string();
        let mut s = Source::new(Some(&raw));

        fs::write(&path, "aaa\n").unwrap();
        s.refresh();
        assert_eq!(s.text(), Some("aaa"));

        // 同长度改写：(大小, mtime) 里大小没变，而同一秒内两次写入的 mtime
        // 可能也相同，所以这一步不保证被察觉——真正要钉住的是下一条规则。
        fs::write(&path, "bbb\n").unwrap();
        s.refresh();
        assert!(s.text() == Some("aaa") || s.text() == Some("bbb"), "两种都可能，取决于时间戳粒度");

        // 长度变了就一定重读
        fs::write(&path, "much longer line\n").unwrap();
        s.refresh();
        assert_eq!(s.text(), Some("much longer line"));

        // 清空文件 → 状态行交还给挂件
        fs::write(&path, "\n").unwrap();
        s.refresh();
        assert_eq!(s.text(), None);

        fs::remove_file(&path).unwrap();
        s.refresh();
        assert_eq!(s.text(), None, "文件消失后必须清空");
        fs::remove_dir_all(&dir).ok();
    }

    /// 只有配了路径才会碰文件系统：指向不存在的路径时 refresh 不该 panic，
    /// 也不该留下任何内容。
    #[test]
    fn missing_file_is_silently_inactive() {
        let mut s = Source::new(Some("/definitely/not/here/tinyticker.txt"));
        s.refresh();
        assert_eq!(s.text(), None);
        assert_eq!(s.stamp, None);
    }

    #[test]
    fn oversized_file_is_refused() {
        let dir = std::env::temp_dir().join("tinyticker-textsrc-big");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.txt");
        // 超一个字节就该整个不读，而不是读一半
        fs::write(&path, vec![b'a'; MAX_BYTES as usize + 1]).unwrap();
        let raw = path.to_str().unwrap().to_string();
        let mut s = Source::new(Some(&raw));
        s.refresh();
        assert_eq!(s.text(), None, "超限文件不该被采信");
        fs::write(&path, vec![b'b'; MAX_BYTES as usize]).unwrap();
        s.refresh();
        assert_eq!(s.text().map(|t| t.len()), Some(MAX_BYTES as usize), "刚好在上限内要能读");
        fs::remove_dir_all(&dir).ok();
    }

}
