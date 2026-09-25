//! 托盘文案的双语层。Catime 的做法照搬：一张文案表 + 语言检测，切换走托盘子菜单
//! 而不是对话框（`window_commands_language.c:16-28`——它也没有语言对话框）。
//!
//! # 只管托盘，不管挂件
//!
//! 挂件那两行字受 GAP.md §零 的 R2 约束：数字行恒为 8×8 点阵（RUNNING / PAUSED /
//! WORK n 这些状态词保持 ASCII，两边通用），状态行的非 ASCII 走宿主字体。
//! 所以本模块覆盖的是：托盘菜单标签、悬停提示、通知文案——全是"宿主画的 UI"。
//!
//! # 线程
//!
//! 语言在启动时按配置落定一次，之后只有托盘菜单「语言」一项能改（经主循环写回
//! 配置再同步到这里）。`AtomicU8` 就够了；菜单节点的标签在**构建时**取当前语言，
//! 切换语言会触发托盘重建节点（`TrayMsg::SyncConfig` 那条路）。

use std::sync::atomic::{AtomicU8, Ordering};

/// 配置项 `language` 的取值。`Auto` 时看环境变量。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Language {
    /// 跟随系统：`$LANGUAGE` / `$LC_ALL` / `$LC_MESSAGES` / `$LANG` 里第一个非空值
    /// 以 `zh` 开头则中文，否则英文。
    #[default]
    Auto,
    Zh,
    En,
}

impl Language {
    pub fn from_name(name: &str) -> Option<Language> {
        match name {
            "auto" => Some(Language::Auto),
            "zh" => Some(Language::Zh),
            "en" => Some(Language::En),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Language::Auto => "auto",
            Language::Zh => "zh",
            Language::En => "en",
        }
    }

    /// 菜单标签。三种取值都给母语名（Catime 同样是"每行用自己的语言写"）。
    pub fn label(self) -> &'static str {
        match self {
            Language::Auto => "自动（跟随系统）",
            Language::Zh => "简体中文",
            Language::En => "English",
        }
    }

    /// 把 `Auto` 落定成具体语言。环境变量按 GNU 的优先级链取第一个非空值。
    pub fn resolve(self) -> Language {
        match self {
            s @ (Language::Zh | Language::En) => s,
            Language::Auto => {
                for var in ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"] {
                    let v = std::env::var(var).unwrap_or_default();
                    if v.is_empty() {
                        continue;
                    }
                    // 值形如 `zh_CN.UTF-8`；`C` / `POSIX` 落英文
                    return if v.starts_with("zh") {
                        Language::Zh
                    } else {
                        Language::En
                    };
                }
                Language::En
            }
        }
    }
}

/// 当前语言（`resolve` 之后的值）。默认中文：冷启动早期（`main` 落定之前）与单测
/// 拿到的文案确定，不受构建机环境变量摆布。
static LANG: AtomicU8 = AtomicU8::new(0);

fn code(lang: Language) -> u8 {
    match lang {
        Language::Zh => 0,
        Language::En | Language::Auto => 1,
    }
}

/// 落定当前语言。启动时一次 + 托盘「语言」菜单项/热加载各一次。
pub fn set(lang: Language) {
    LANG.store(code(lang.resolve()), Ordering::Relaxed);
}

/// 显式语言版词条。**菜单节点的标签全走这一条**：`build_nodes` 收一个 `Language`
/// 参数，于是单测断言具体字面量时不必碰进程级全局，并行测试也不会互相掰语言。
#[must_use]
pub fn tr_in<'a>(lang: Language, zh: &'a str, en: &'a str) -> &'a str {
    match code(lang) {
        0 => zh,
        _ => en,
    }
}

/// 双语词条：`tr("开始", "Start")`，取当前全局语言。托盘外的文案（通知正文）
/// 走这条——它们不参与 `build_nodes` 的确定性断言。
#[must_use]
pub fn tr<'a>(zh: &'a str, en: &'a str) -> &'a str {
    // 参数名 `zh` 会把同名函数遮蔽掉，这里直接读原子量
    if LANG.load(Ordering::Relaxed) == 0 {
        zh
    } else {
        en
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试并行跑，而 `LANG` 是进程级全局：**单测一律不碰 `set()`**，只测纯函数。
    /// 谁在测试里翻全局，就会把其它线程里断言中文标签的测试（如托盘的
    /// `tooltip_lists_what_was_sampled`）变成随机爆——这条戒律写在这里免得有人再踩。
    #[test]
    fn names_roundtrip() {
        for l in [Language::Auto, Language::Zh, Language::En] {
            assert_eq!(Language::from_name(l.name()), Some(l));
        }
        assert_eq!(Language::from_name("de"), None);
        assert_eq!(Language::default(), Language::Auto, "默认该是跟随系统");
    }

    /// `resolve` 只做"zh 前缀"判定，且显式值不被环境变量绑架。
    #[test]
    fn explicit_beats_env() {
        assert_eq!(Language::Zh.resolve(), Language::Zh);
        assert_eq!(Language::En.resolve(), Language::En);
    }

    /// 词条表本身不许有空串，且两种语言的取值要分开（显式版不读全局，
    /// 因此这条测试与其它并行跑的标签测试互不干扰）。
    #[test]
    fn tr_in_is_pure() {
        assert_eq!(tr_in(Language::Zh, "开始", "Start"), "开始");
        assert_eq!(tr_in(Language::En, "开始", "Start"), "Start");
        // Auto 在 tr_in 里按英文处理：它只该在进菜单前就被 `resolve` 换掉
        assert_eq!(tr_in(Language::Auto, "开始", "Start"), "Start");
        for l in [Language::Auto, Language::Zh, Language::En] {
            assert!(!l.label().is_empty());
        }
    }
}
