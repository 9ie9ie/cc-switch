# CC Switch GPT-5.6 Pricing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 CC Switch v3.16.5 reasoning-token 自构建版中加入 GPT-5.6 Sol、Terra、Luna 的官方四维 token 定价，并重新构建、部署到 E 盘。

**Architecture:** 继续使用 `Database::seed_model_pricing` 的内置价格表和 `INSERT OR IGNORE` 启动补种机制，不提升数据库版本，也不覆盖同 ID 的用户自定义价格。模型别名继续使用现有 `model_pricing_candidates` 归一化流程；GitHub Actions 负责 Rust 验证和 Windows 构建，最终只替换 E 盘当前自构建版可执行文件。

**Tech Stack:** Rust、rusqlite、Tauri 2、Vitest、TypeScript、GitHub Actions、PowerShell

## Global Constraints

- 价格单位固定为 USD/1M tokens。
- `gpt-5.6-sol`: input `5`、output `30`、cache read `0.5`、cache creation `6.25`。
- `gpt-5.6-terra`: input `2.5`、output `15`、cache read `0.25`、cache creation `3.125`。
- `gpt-5.6-luna`: input `1`、output `6`、cache read `0.1`、cache creation `1.25`。
- 不修改 `SCHEMA_VERSION`，不新增数据库迁移。
- 使用 `INSERT OR IGNORE` 增量补种；现有同 ID 用户价格必须保持不变。
- 不修改模型归一化生产代码。
- 构建产物和安装位置不得放到 C 盘；目标程序固定为 `E:\INSTALL\CC Switch Reasoning 3.16.5\cc-switch.exe`。
- 保留 `E:\INSTALL\CC Switch Reasoning\cc-switch.exe` 和 `E:\INSTALL\CC Switch\cc-switch.exe` 两个旧版本。

---

## File Structure

- `src-tauri/src/database/schema.rs`: 增加三个 GPT-5.6 内置定价行。
- `src-tauri/src/database/tests.rs`: 验证四维价格、启动补种和用户价格不覆盖。
- `src-tauri/src/services/usage_stats.rs`: 验证 provider、日期和 effort 后缀别名能够命中新价格。
- `.github/workflows/personal-windows-build.yml`: 在现有 reasoning 测试之外运行 pricing 相关 Rust 测试。
- `.superpowers/sdd/progress.md`: 由控制器记录任务状态，不纳入功能提交。

---

### Task 1: Add and verify GPT-5.6 pricing

**Files:**
- Modify: `src-tauri/src/database/schema.rs:1439`
- Modify: `src-tauri/src/database/tests.rs:688`
- Modify: `src-tauri/src/services/usage_stats.rs:3965`
- Modify: `.github/workflows/personal-windows-build.yml:35`

**Interfaces:**
- Consumes: `Database::seed_model_pricing`, `Database::ensure_model_pricing_seeded`, `find_model_pricing_row`。
- Produces: 三个 `model_pricing` 内置行；已有查价接口不变。

- [ ] **Step 1: Add failing schema assertions**

在 `schema_model_pricing_is_seeded_on_init` 的 GPT 数量断言之后加入以下代码。当前实现缺少三行，因此查询会以 `query GPT-5.6 price` 失败：

```rust
    for (model_id, expected) in [
        (
            "gpt-5.6-sol",
            ("GPT-5.6 Sol", "5", "30", "0.5", "6.25"),
        ),
        (
            "gpt-5.6-terra",
            ("GPT-5.6 Terra", "2.5", "15", "0.25", "3.125"),
        ),
        (
            "gpt-5.6-luna",
            ("GPT-5.6 Luna", "1", "6", "0.1", "1.25"),
        ),
    ] {
        let actual: (String, String, String, String, String) = conn
            .query_row(
                "SELECT display_name, input_cost_per_million, output_cost_per_million,
                        cache_read_cost_per_million, cache_creation_cost_per_million
                 FROM model_pricing WHERE model_id = ?1",
                [model_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("query GPT-5.6 price");
        assert_eq!(
            actual,
            (
                expected.0.to_string(),
                expected.1.to_string(),
                expected.2.to_string(),
                expected.3.to_string(),
                expected.4.to_string(),
            ),
            "{model_id} should use the official built-in price"
        );
    }
```

- [ ] **Step 2: Add a failing incremental-seed preservation test**

在 `model_pricing_seed_repairs_known_outdated_builtin_prices` 前加入：

```rust
#[test]
fn model_pricing_seed_preserves_existing_gpt_5_6_user_price() {
    let db = Database::memory().expect("create memory db");

    {
        let conn = db.conn.lock().expect("lock conn");
        conn.execute(
            "INSERT OR REPLACE INTO model_pricing (
                model_id, display_name, input_cost_per_million, output_cost_per_million,
                cache_read_cost_per_million, cache_creation_cost_per_million
             ) VALUES ('gpt-5.6-sol', 'Custom GPT-5.6 Sol', '9', '99', '0.9', '9.9')",
            [],
        )
        .expect("set custom GPT-5.6 Sol price");
        conn.execute("DELETE FROM model_pricing WHERE model_id = 'gpt-5.6-terra'", [])
            .expect("remove GPT-5.6 Terra price");
    }

    db.ensure_model_pricing_seeded()
        .expect("ensure pricing seeded");

    let conn = db.conn.lock().expect("lock conn");
    let sol: (String, String, String, String, String) = conn
        .query_row(
            "SELECT display_name, input_cost_per_million, output_cost_per_million,
                    cache_read_cost_per_million, cache_creation_cost_per_million
             FROM model_pricing WHERE model_id = 'gpt-5.6-sol'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .expect("query custom GPT-5.6 Sol price");
    assert_eq!(
        sol,
        (
            "Custom GPT-5.6 Sol".to_string(),
            "9".to_string(),
            "99".to_string(),
            "0.9".to_string(),
            "9.9".to_string(),
        )
    );

    let terra: (String, String, String, String) = conn
        .query_row(
            "SELECT input_cost_per_million, output_cost_per_million,
                    cache_read_cost_per_million, cache_creation_cost_per_million
             FROM model_pricing WHERE model_id = 'gpt-5.6-terra'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("query reseeded GPT-5.6 Terra price");
    assert_eq!(
        terra,
        (
            "2.5".to_string(),
            "15".to_string(),
            "0.25".to_string(),
            "3.125".to_string(),
        )
    );
}
```

- [ ] **Step 3: Add failing alias matching cases**

在 `test_model_pricing_matching` 的 GPT-5.5 日期后缀断言之后加入：

```rust
        for (alias, expected) in [
            (
                "openai/gpt-5.6-sol:priority",
                ("5", "30", "0.5", "6.25"),
            ),
            (
                "OpenAI/GPT-5.6-Terra-2026-06-26",
                ("2.5", "15", "0.25", "3.125"),
            ),
            (
                "gpt-5.6-luna@xhigh",
                ("1", "6", "0.1", "1.25"),
            ),
        ] {
            let actual = find_model_pricing_row(&conn, alias)?
                .unwrap_or_else(|| panic!("{alias} should resolve to GPT-5.6 pricing"));
            assert_eq!(
                actual,
                (
                    expected.0.to_string(),
                    expected.1.to_string(),
                    expected.2.to_string(),
                    expected.3.to_string(),
                ),
                "{alias} should resolve to the expected GPT-5.6 price"
            );
        }
```

- [ ] **Step 4: Verify the new tests are red before implementation**

本机没有 Cargo，不安装到 C 盘。通过代码审查确认三个 seed 查询当前不存在；不要先改生产代码。测试将在本任务推送后的 Windows Actions 中实际执行。

- [ ] **Step 5: Add the minimal pricing rows**

在 `schema.rs` 的 `// GPT-5.5 系列` 前加入：

```rust
            // GPT-5.6 系列
            ("gpt-5.6-sol", "GPT-5.6 Sol", "5", "30", "0.5", "6.25"),
            (
                "gpt-5.6-terra",
                "GPT-5.6 Terra",
                "2.5",
                "15",
                "0.25",
                "3.125",
            ),
            (
                "gpt-5.6-luna",
                "GPT-5.6 Luna",
                "1",
                "6",
                "0.1",
                "1.25",
            ),
```

- [ ] **Step 6: Make Actions run pricing tests**

把工作流的 Rust test step 改为：

```yaml
      - name: Rust tests
        working-directory: src-tauri
        run: |
          cargo test reasoning
          cargo test model_pricing
```

- [ ] **Step 7: Run local non-Rust checks**

Run:

```powershell
.\node_modules\.bin\vitest.cmd run tests/components/RequestLogTable.test.tsx
.\node_modules\.bin\tsc.cmd --noEmit
git diff --check
```

Expected: Vitest 通过，TypeScript 退出码 `0`，`git diff --check` 无输出。

- [ ] **Step 8: Commit and push**

```powershell
git add -- src-tauri/src/database/schema.rs src-tauri/src/database/tests.rs src-tauri/src/services/usage_stats.rs .github/workflows/personal-windows-build.yml
git diff --cached --check
git commit -m "feat: add GPT-5.6 model pricing"
git push fork codex/reasoning-token-display-v3.16.5
```

Expected: 功能提交成功并触发 `Personal Windows Build`。

- [ ] **Step 9: Verify GitHub Actions**

```powershell
$run = gh run list --repo 9ie9ie/cc-switch --workflow personal-windows-build.yml --branch codex/reasoning-token-display-v3.16.5 --limit 1 --json databaseId,status,conclusion,headSha | ConvertFrom-Json
gh run watch $run.databaseId --repo 9ie9ie/cc-switch --exit-status
gh run view $run.databaseId --repo 9ie9ie/cc-switch
```

Expected: Typecheck、frontend tests、`cargo test reasoning`、`cargo test model_pricing`、NSIS build 和 artifact upload 全部成功。

---

### Task 2: Deploy and verify the rebuilt E-drive app

**Files:**
- Read: GitHub Actions artifact `cc-switch-windows-x64-v3.16.5-reasoning-token`
- Back up: `C:\Users\13164\.cc-switch\cc-switch.db`
- Replace: `E:\INSTALL\CC Switch Reasoning 3.16.5\cc-switch.exe`

**Interfaces:**
- Consumes: Task 1 成功的 Actions run 和 artifact。
- Produces: E 盘运行中的新版 CC Switch；live v12 数据库包含三个 GPT-5.6 定价行。

- [ ] **Step 1: Download the artifact to D drive**

```powershell
$repo = '9ie9ie/cc-switch'
$branch = 'codex/reasoning-token-display-v3.16.5'
$run = gh run list --repo $repo --workflow personal-windows-build.yml --branch $branch --limit 1 --json databaseId,status,conclusion,headSha | ConvertFrom-Json
if ($run.conclusion -ne 'success') { throw "Actions run $($run.databaseId) is not successful" }
$artifact = (gh api "repos/$repo/actions/runs/$($run.databaseId)/artifacts" | ConvertFrom-Json).artifacts | Where-Object name -eq 'cc-switch-windows-x64-v3.16.5-reasoning-token' | Select-Object -First 1
if (-not $artifact) { throw 'Windows artifact not found' }
$root = "D:\ALL\py_script\Claude_retry\Codex\artifacts\cc-switch-v3.16.5-run-$($run.databaseId)"
New-Item -ItemType Directory -Force -Path $root | Out-Null
$zip = Join-Path $root 'cc-switch-windows-x64-v3.16.5-reasoning-token.zip'
$token = gh auth token
curl.exe -L -H "Authorization: Bearer $token" -H "Accept: application/vnd.github+json" "https://api.github.com/repos/$repo/actions/artifacts/$($artifact.id)/zip" -o $zip
$extract = Join-Path $root 'artifact'
New-Item -ItemType Directory -Force -Path $extract | Out-Null
Expand-Archive -LiteralPath $zip -DestinationPath $extract -Force
Get-FileHash -Algorithm SHA256 $zip
Get-ChildItem -Path $extract -Recurse -Filter cc-switch.exe
```

Expected: ZIP 和 `cc-switch.exe` 都位于 D 盘 artifact 目录。

- [ ] **Step 2: Back up the live database to D drive**

```powershell
$stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
$backupDir = Join-Path $root "database-backup-$stamp"
New-Item -ItemType Directory -Force -Path $backupDir | Out-Null
Copy-Item -LiteralPath 'C:\Users\13164\.cc-switch\cc-switch.db' -Destination (Join-Path $backupDir 'cc-switch.db')
Get-FileHash -Algorithm SHA256 (Join-Path $backupDir 'cc-switch.db')
```

Expected: D 盘备份存在且 SHA256 可读取。

- [ ] **Step 3: Replace only the current E-drive executable**

```powershell
$target = 'E:\INSTALL\CC Switch Reasoning 3.16.5\cc-switch.exe'
$resolvedTarget = [System.IO.Path]::GetFullPath($target)
if (-not $resolvedTarget.StartsWith('E:\INSTALL\CC Switch Reasoning 3.16.5\', [System.StringComparison]::OrdinalIgnoreCase)) { throw 'Unexpected deployment target' }
Get-Process cc-switch -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $target } | Stop-Process
$builtExe = Get-ChildItem -Path $extract -Recurse -Filter cc-switch.exe | Where-Object { $_.FullName -notlike '*bundle*' } | Select-Object -First 1
if (-not $builtExe) { throw 'Portable cc-switch.exe not found' }
Copy-Item -LiteralPath $builtExe.FullName -Destination $target -Force
Get-FileHash -Algorithm SHA256 $target
Start-Process -FilePath $target
Start-Sleep -Seconds 8
Get-Process cc-switch | Where-Object { $_.Path -eq $target } | Select-Object Id,Path,Responding,MainWindowTitle
```

Expected: 只有 `E:\INSTALL\CC Switch Reasoning 3.16.5\cc-switch.exe` 被替换；进程路径为 E 盘且 `Responding` 为 `True`。

- [ ] **Step 4: Verify live pricing rows**

```powershell
@'
import sqlite3

db = sqlite3.connect(r"C:\Users\13164\.cc-switch\cc-switch.db")
rows = db.execute(
    """SELECT model_id, display_name, input_cost_per_million,
              output_cost_per_million, cache_read_cost_per_million,
              cache_creation_cost_per_million
       FROM model_pricing
       WHERE model_id IN ('gpt-5.6-sol', 'gpt-5.6-terra', 'gpt-5.6-luna')
       ORDER BY model_id"""
).fetchall()
expected = [
    ('gpt-5.6-luna', 'GPT-5.6 Luna', '1', '6', '0.1', '1.25'),
    ('gpt-5.6-sol', 'GPT-5.6 Sol', '5', '30', '0.5', '6.25'),
    ('gpt-5.6-terra', 'GPT-5.6 Terra', '2.5', '15', '0.25', '3.125'),
]
assert rows == expected, rows
print(rows)
print('user_version=', db.execute('PRAGMA user_version').fetchone()[0])
'@ | python -
```

Expected: 三行与 `expected` 完全一致，`user_version= 12`。

- [ ] **Step 5: Verify no unintended installation changes**

```powershell
Get-Item 'E:\INSTALL\CC Switch Reasoning\cc-switch.exe','E:\INSTALL\CC Switch\cc-switch.exe' | Select-Object FullName,Length,LastWriteTime
git status --short
```

Expected: 两个旧版本仍存在；源代码工作树只允许出现 `.superpowers/sdd` 的忽略状态文件，不允许出现未提交源码改动。
