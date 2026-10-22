# 自动更新测试用的签名文件

`src-tauri/src/updater/tests.rs` 用这里的文件验证“签名对的更新能下载并通过验签、签名不对的会被拒绝”。

- `test.key.pub`：**测试用**的更新公钥。对应的私钥没有放进仓库，也不是正式发布用的那把。
- `payload.bin` / `payload.bin.sig`：一个假的“更新包”和测试私钥对它的签名。
- `other.bin` / `other.bin.sig`：另一个文件和它的签名（用来造“签的是另一个文件”）。
- `payload.attacker.sig`：**另一把**私钥对 `payload.bin` 的签名（用来造“别人的私钥签的”）。

重新生成（需要 tauri-cli）：

```powershell
cargo tauri signer generate --ci -p "" -w <仓库外的目录>	est.key      # 测试密钥对，把 .pub 复制到这里
cargo tauri signer sign -f <仓库外的目录>	est.key -p "" payload.bin    # 得到 payload.bin.sig
cargo tauri signer sign -f <仓库外的目录>	est.key -p "" other.bin      # 得到 other.bin.sig
cargo tauri signer generate --ci -p "" -w <仓库外的目录>ttacker.key   # 另一把
# 用 attacker.key 给 payload.bin 签名，得到的 .sig 改名为 payload.attacker.sig
```

**不要把这里的测试公钥当成正式公钥**；正式密钥的生成步骤见 `docs/release.md`。
