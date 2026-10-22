#!/usr/bin/env bash
# 五条门禁，逐条报告退出码和耗时。用法：bash scripts/gates.sh
cd "$(dirname "$0")/.."
total_start=$SECONDS; fail=0
# 系统临时目录里 zssh* 开头的目录和文件（.log 除外）：开始和结束各数一次，多出来的就是测试没删干净的。
temp_root="$(cygpath -u "${TEMP:-${TMPDIR:-/tmp}}" 2>/dev/null || echo "${TMPDIR:-/tmp}")"
list_temp() { find "$temp_root" -maxdepth 1 -mindepth 1 -name 'zssh*' ! -name '*.log' 2>/dev/null | sort; }
temp_before="$(list_temp)"
echo "临时目录（zssh* 目录）开始时：$(printf '%s' "$temp_before" | grep -c .) 个"
run() { local name="$1"; shift; local s=$SECONDS; "$@" > /tmp/gate-last.log 2>&1; local c=$?; echo "[$name] 退出码 $c  耗时 $((SECONDS-s)) 秒"; [ $c -ne 0 ] && { fail=1; tail -30 /tmp/gate-last.log; }; return 0; }
run "cargo fmt" cargo fmt --all -- --check
run "cargo clippy" cargo clippy --workspace --all-targets -- -D warnings
run "cargo test" cargo test --workspace
cp /tmp/gate-last.log /tmp/gate-cargo-test.log 2>/dev/null
run "npm run build" bash -c 'cd ui && npm run build'
run "npm test" bash -c 'cd ui && npm test'
temp_after="$(list_temp)"
temp_new="$(comm -13 <(printf '%s
' "$temp_before") <(printf '%s
' "$temp_after") | grep .)"
echo "临时目录结束时：$(printf '%s' "$temp_after" | grep -c .) 个（多出 $(printf '%s' "$temp_new" | grep -c .) 个）"
if [ -n "$temp_new" ]; then fail=1; echo "测试留下了没删的临时目录："; printf '%s
' "$temp_new" | sed 's/^/  /'; fi
echo "总耗时 $((SECONDS-total_start)) 秒"
exit $fail
