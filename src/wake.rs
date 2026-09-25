//! self-pipe 唤醒器：让阻塞在 `poll` 上的事件循环立刻看到新到达的通道命令。
//!
//! # 为什么需要它
//!
//! 心跳阶梯下探到 1 秒（空闲档）之后，`poll` 的超时可以长到 1000ms。托盘菜单点击、
//! 套接字转发进来的命令走的是 `mpsc::Channel`——它不占用任何 fd，`poll` 等不到它，
//! 于是"绑一条 `tinyticker 25m` 当快捷键"的反应会慢达一秒。
//!
//! 解法是经典的 self-pipe：发送方在 `send` 之后往管道里写一个字节，事件循环把
//! 管道的读端与 Wayland / X11 的连接 fd 一起 `poll`，读到就排空。用
//! `UnixStream::pair()` 而不是 `pipe(2)`：std 自带，不必为一次 `pipe` 再开一组 FFI。
//!
//! 丢唤醒窗口不存在：字节会一直留在管道里，`poll` 对"可读的 socket"立即返回；
//! 写端在满时得到 `WouldBlock`，直接忽略即可——读端已有待处理字节，说明循环还没睡。

use std::io::{ErrorKind, Read, Write};
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;

/// 建一对（发送端，接收端）。接收端交给事件循环，发送端 `clone` 给每个会向
/// 主循环 `send` 命令的线程。
pub fn pair() -> (WakeSender, WakeReader) {
    let (tx, rx) = UnixStream::pair().expect("UnixStream::pair 不会失败（fd 充足时）");
    let _ = tx.set_nonblocking(true);
    let _ = rx.set_nonblocking(true);
    (WakeSender { tx }, WakeReader { rx })
}

/// 发送端：写一个字节唤醒事件循环。可跨线程 `clone`。
pub struct WakeSender {
    tx: UnixStream,
}

impl WakeSender {
    /// 唤醒一次。忽略所有错误：管道满（前一个字节还没被读走）本身就够了。
    pub fn wake(&self) {
        let mut s = &self.tx;
        let _ = s.write_all(&[1]);
        let _ = s.flush();
    }
}

impl Clone for WakeSender {
    fn clone(&self) -> Self {
        match self.tx.try_clone() {
            Ok(tx) => Self { tx },
            // try_clone 失败（fd 耗尽）时退化为"不唤醒"：行为回到按心跳节拍取命令，
            // 延迟有界（≤1s），不能让唤醒机制自己把程序弄崩
            Err(_) => {
                eprintln!("⚠️ 唤醒管道克隆失败，托盘/套接字命令最多延迟一个心跳");
                let (tx, _rx) = UnixStream::pair().expect("同上");
                let _ = tx.set_nonblocking(true);
                Self { tx }
            }
        }
    }
}

/// 接收端：fd 交给 `poll`，每轮循环开头 [`WakeReader::drain`] 排空。
pub struct WakeReader {
    rx: UnixStream,
}

impl WakeReader {
    pub fn fd(&self) -> RawFd {
        self.rx.as_raw_fd()
    }

    /// 排空累积的唤醒字节。读到 `WouldBlock`/EOF 就停——不阻塞是底线。
    pub fn drain(&self) {
        let mut s = &self.rx;
        let mut buf = [0u8; 64];
        loop {
            match s.read(&mut buf) {
                Ok(0) => return,
                Ok(_) => continue,
                Err(e)
                    if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::Interrupted =>
                {
                    return;
                }
                Err(_) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 写过的唤醒必须让读端立刻可读（这是 `poll` 提前返回的前提），排空后又回到不可读。
    #[test]
    fn wake_makes_the_reader_ready_and_drain_empties_it() {
        let (tx, rx) = pair();
        assert!(!ready(&rx));
        tx.wake();
        tx.wake();
        assert!(ready(&rx), "两次 wake 之后读端该立刻可读");
        rx.drain();
        assert!(!ready(&rx), "drain 之后该回到不可读");
        // clone 出来的发送端独立可用
        let tx2 = tx.clone();
        tx2.wake();
        assert!(ready(&rx));
        rx.drain();
    }

    /// 非阻塞读一次，问"现在有没有字节"。
    fn ready(rx: &WakeReader) -> bool {
        let mut s = &rx.rx;
        let mut buf = [0u8; 8];
        matches!(s.read(&mut buf), Ok(n) if n > 0)
    }

    /// 灌远超缓冲的字节数：`wake` 不许 panic（满即 `WouldBlock`，被忽略），
    /// 而且读端仍可排空。
    #[test]
    fn flooding_the_pipe_never_panics() {
        let (tx, rx) = pair();
        for _ in 0..100_000 {
            tx.wake();
        }
        rx.drain();
    }
}
