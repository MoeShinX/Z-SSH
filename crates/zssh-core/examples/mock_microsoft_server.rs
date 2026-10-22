//! 截图 / 端到端脚本用的本地模拟微软授权服务器 + OneDrive 接口，监听 127.0.0.1 的随机端口，
//! 打印 `READY <端口>` 之后一直跑到被结束。数据只在内存里，不碰真实的微软服务。
//!
//! client ID 是 `11111111-2222-3333-4444-555555555555`（假的），登录后的账号名是 `Test User`。
//! 授权页会直接同意（302 回应用的回环地址）；令牌、授权码都是 `mock-…` 开头的假值。

#[path = "../tests/common/mock_microsoft.rs"]
mod mock_microsoft;

use mock_microsoft::MockMicrosoft;

#[tokio::main]
async fn main() {
    let microsoft = MockMicrosoft::start().await;
    println!("READY {}", microsoft.addr().port());
    // 一直跑到被结束（脚本只结束自己启动的这个进程）。
    std::future::pending::<()>().await;
}
