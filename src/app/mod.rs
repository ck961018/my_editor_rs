//! 应用编排层：连接编辑核心、共享协议和前端抽象，不依赖具体 TUI/GUI 实现。
//!
//! `application` 定义稳定的 `App` 入口，`runtime`、`save`、`layout` 和 `query`
//! 分别承载事件循环、后台保存、Scene/View 生命周期和前端查询适配。

mod application;
mod dispatcher;
mod kernel;
mod layout;
mod message;
mod query;
mod remote;
mod runtime;
mod save;
mod scene_model;
mod session;
mod tasks;
mod view;

pub use application::App;

#[cfg(test)]
mod tests;
