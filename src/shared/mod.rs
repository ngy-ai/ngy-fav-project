//! 与领域无关的通用能力：时间格式化、阻塞 HTTP 的异步桥接、数据位置与搬家、
//! 可选中复制的文本。

pub mod clock;
pub mod net;
pub mod relocate;
pub mod store;
pub mod text;
