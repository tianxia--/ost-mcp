# ost-mcp

Query an Outlook mailbox file over MCP, in place.

```text
model  <-->  ost-mcp (MCP stdio + DuckDB)  <-->  .ost / Outlook.sqlite
```

Point it at an `.ost`, a `.pst`, or a Mac Outlook profile, and a model can search
it with SQL, read message bodies, and pull attachment payloads. Nothing is
exported, converted or indexed: DuckDB table functions read the store as each
query runs, the mapping is read-only, and **Outlook can stay open** while you
query.

It is a single binary in pure Rust with the DuckDB amalgamation compiled in, so
there is nothing to install alongside it.

## 免责声明 / Disclaimer

本项目基于 [3rg0n/ost-mcp](https://github.com/3rg0n/ost-mcp)（作者 Ergon Copeland，MIT
协议）修改而来，原作者的版权声明保留在 [LICENSE](LICENSE) 中。本仓库与原作者没有关联，
也未获其认可；问题请在本仓库反馈，不要提交给上游。

本软件按“原样”提供，不附带任何明示或暗示的担保。它会读取你本机的真实邮件，使用风险由你自行
承担，作者和贡献者不对因使用本软件造成的任何损失负责。

This project is based on, and modified from,
[3rg0n/ost-mcp](https://github.com/3rg0n/ost-mcp) by Ergon Copeland (MIT); the
original copyright notice is kept in [LICENSE](LICENSE). It is not affiliated with
or endorsed by the original author — report issues here, not upstream. The
software is provided "as is", without warranty of any kind; it reads your real
mail, and you use it at your own risk.

## 中文使用指南

ost-mcp 直接读取本机 Outlook 的邮箱文件，让 Claude（或任何 MCP 客户端、脚本）用 SQL
查邮件、读正文、取附件。**全程只读**，不导出、不建索引，Outlook 开着也能用，不需要登录
账号，也不走 Graph API。

### 1. 适用范围

| 平台 | 读取的数据 | 说明 |
|---|---|---|
| Windows | `.ost`（Exchange/M365 缓存模式）、`.pst` | 自动从注册表找到默认配置文件的邮箱 |
| macOS | Outlook 配置文件（`Outlook.sqlite` + `.olk15*` + `HxStore.hxd`） | 自动在 `~/Library/Group Containers/UBF8T346G9.Office/Outlook` 下查找 |
| Linux | 不支持 | Outlook 不在 Linux 上运行，没有可读的邮箱 |

### 2. 安装

**方式 A：下载本仓库 Release 里的预编译版本（macOS Apple 芯片）**

本仓库是私有仓库，用 `gh` 下载（需要已 `gh auth login` 且有仓库权限）：

```sh
mkdir -p ~/ost-mcp-dl && cd ~/ost-mcp-dl
gh release download v0.2.0 -R tianxia--/ost-mcp -p 'ost-mcp-aarch64-apple-darwin.tar.gz*'

# 校验 SHA-256，输出 OK 才继续
[ "$(shasum -a 256 ost-mcp-aarch64-apple-darwin.tar.gz | cut -d' ' -f1)" = \
  "$(cat ost-mcp-aarch64-apple-darwin.tar.gz.sha256)" ] && echo OK

mkdir -p ~/.local/bin
tar -xzf ost-mcp-aarch64-apple-darwin.tar.gz -C ~/.local/bin
ost-mcp --version        # 应输出 ost-mcp 0.2.0
```

`~/.local/bin` 不在 `PATH` 里时，在 `~/.zshrc` 加一行
`export PATH="$HOME/.local/bin:$PATH"`，然后重开终端。

Windows 版本会在 GitHub Actions 构建完成后出现在同一个 Release 里，文件名
`ost-mcp-x86_64-pc-windows-msvc.zip`，解压出 `ost-mcp.exe` 放到任意 `PATH` 目录即可。

**方式 B：从源码编译**（需要 Rust；Windows 还需要 MSVC 生成工具。首次编译 DuckDB 约 3–10 分钟）

```sh
git clone https://github.com/tianxia--/ost-mcp.git
cd ost-mcp
cargo install --path crates/ost-mcp --locked    # 装到 ~/.cargo/bin/ost-mcp
```

**方式 C：一键安装脚本**——见下文英文部分的 [Install](#install)，脚本已指向本仓库。
注意：**本仓库为私有时，一键安装用不了**——匿名的 `curl` 拿不到私有仓库的脚本、Release
和 skill 文件，会返回 404。仓库私有期间请用方式 A 或方式 B。

### 3. 确认能用

按顺序执行这三条，都正常输出就说明装好了：

```sh
ost-mcp --version    # 版本号
ost-mcp --list       # 本机找到的邮箱：大小、来源、账号、路径
ost-mcp --info       # 默认邮箱的路径、类型(kind)、文件夹数、可查询的表、本地时区
```

`--info` 里的 `kind` 表示读到的是哪种邮箱：`ost-v36`（Outlook 2013 及以后的 OST）、
`ost-v23`（PST）、`mac-olk15`（Mac Outlook）。

想读其他邮箱，把路径作为第一个参数传进去：

```sh
ost-mcp "D:\备份\Archive.pst" --info                     # Windows 上的 PST
ost-mcp "$HOME/Library/Group Containers/UBF8T346G9.Office/Outlook/Outlook 15 Profiles/Main Identity/Data" --info
```

Mac 上的路径可以是配置文件的 `Data` 目录、里面的 `Outlook.sqlite`，或者 `Data` 上一级的目录，
三种都行。

### 4. 接入 Claude

有两种接法，选一种即可。

**接法一：Claude Code Skill（推荐）**

不用改配置，平时不占上下文，只有问到邮件时才加载。

```sh
cp -r skills/ost-mcp ~/.claude/skills/        # 所有项目可用
# 或者只给某个项目用：cp -r skills/ost-mcp <项目>/.claude/skills/
```

重启 Claude Code 会话后，直接用中文提问即可，例如：

- “我收件箱里最近 20 封邮件是谁发的？”
- “这个月谁给我发邮件最多？列前 10。”
- “找一下主题里带‘发票’的邮件，把最新那封的附件存到 /tmp。”
- “哪些文件夹有未读邮件？”

如果 `ost-mcp` 不在 `PATH` 上，设置环境变量 `OST_MCP_BIN` 指向它的完整路径。

**接法二：MCP 服务器**

Claude Code：

```sh
claude mcp add --scope user ost -- "$(command -v ost-mcp)"
# 指定某个邮箱文件：
claude mcp add --scope user ost -- "$(command -v ost-mcp)" "/path/to/archive.pst"
```

Claude Desktop：编辑配置文件
（macOS：`~/Library/Application Support/Claude/claude_desktop_config.json`；
Windows：`%APPDATA%\Claude\claude_desktop_config.json`），加入下面内容后重启 Claude Desktop。
`command` 必须写**绝对路径**，因为桌面应用不读你 shell 里的 `PATH`。

```json
{
  "mcpServers": {
    "ost": {
      "command": "/Users/<你的用户名>/.local/bin/ost-mcp",
      "args": []
    }
  }
}
```

MCP 方式提供的工具：

| 工具 | 参数 | 作用 |
|---|---|---|
| `store_info` | 无 | 邮箱路径、类型、文件夹数、表结构、`utc_offset`、`local_now` |
| `list_folders` | `contains`（可选，按路径模糊匹配） | 文件夹树，含邮件数和未读数 |
| `search` | `text` `folder` `since` `until` `with_attachments` `limit` | 按主题/发件人、文件夹、日期（`YYYY-MM-DD`）、是否有附件搜索，新的在前；`limit` 默认 200，最大 5000 |
| `sql` | `query`、`limit` | 任意一条只读 SQL，`search` 表达不了的统计、分组都用它 |
| `get_message` | `nid`、`max_body_chars`（默认 20000） | 读一封邮件：发件人、收件人、主题、时间、正文、附件列表 |
| `list_attachments` | `nid` | 附件的文件名、类型、大小（不读内容） |
| `read_attachment` | `message_nid` `attachment_nid` `max_bytes`（默认 1 MB） | 读一个附件：文本直接返回，其他返回 base64 |

注意：`search` 只匹配主题、发件人名和发件人地址，**不搜正文**。

### 5. 命令行用法

每个 MCP 工具都有对应的命令行参数，输出相同的 JSON，适合脚本里用：

| 命令 | 作用 |
|---|---|
| `ost-mcp --list` | 列出找到的所有邮箱 |
| `ost-mcp [邮箱] --info` | 邮箱信息和表结构 |
| `ost-mcp [邮箱] --sql "<SQL>"` | 执行一条只读 SQL，输出结果行（`--limit` 默认 10000） |
| `ost-mcp [邮箱] --message <nid>` | 读一封邮件（`--max-body-chars` 默认 20000） |
| `ost-mcp [邮箱] --attachments <nid>` | 列出一封邮件的附件 |
| `ost-mcp [邮箱] --attachment <邮件nid>:<附件nid>` | 读一个附件，文本或 base64（`--max-bytes` 默认 1048576） |
| `ost-mcp [邮箱] --attachment <邮件nid>:<附件nid> --out <文件>` | 把附件原样写到文件 |
| `ost-mcp`（不带操作参数） | 以 stdio 方式启动 MCP 服务器（给 Claude 用，不要手动在终端里跑） |

可以查询的表：

```sql
folders(nid, parent_nid, name, path, item_count, unread_count,
        has_subfolders, is_search_folder)

messages(nid, folder_nid, folder_path, subject, sender_name, sender_email,
         delivered, submitted, modified, size, unread, has_attachments,
         message_class)

ost_attachments(message_nid => <nid>)   -- 表函数，必须给一个邮件 nid
  -> (message_nid, nid, filename, mime, content_id, declared_size, data_len)
```

`nid` 是一切操作的句柄：邮件的 `nid` 从查询结果里来，附件的 `nid` 从 `--attachments` 里来。

**常用查询**

```sh
# 收件箱最近 20 封
ost-mcp --sql "SELECT nid, delivered, sender_name, sender_email, subject
               FROM messages WHERE folder_path ILIKE '%Inbox%'
               ORDER BY delivered DESC LIMIT 20"

# 谁发邮件最多
ost-mcp --sql "SELECT sender_email, count(*) AS n FROM messages
               WHERE sender_email IS NOT NULL
               GROUP BY 1 ORDER BY n DESC LIMIT 10"

# 每个文件夹的未读数
ost-mcp --sql "SELECT path, item_count, unread_count FROM folders
               WHERE unread_count > 0 ORDER BY unread_count DESC"

# 某段时间内、带附件的邮件（时间按本机时区）
ost-mcp --sql "SELECT nid, delivered, subject FROM messages
               WHERE has_attachments
                 AND delivered >= TIMESTAMP '2026-11-01'
                 AND delivered <  TIMESTAMP '2026-12-01'
               ORDER BY delivered DESC LIMIT 50"

# 主题关键字搜索
ost-mcp --sql "SELECT nid, delivered, subject FROM messages
               WHERE subject ILIKE '%发票%' ORDER BY delivered DESC LIMIT 10"
```

**读一封邮件、取附件**

```sh
ost-mcp --message 24295876                          # 用上面查到的 nid
ost-mcp --attachments 24295876                      # 看附件列表，拿到附件 nid
ost-mcp --attachment 24295876:24277637 --out /tmp/report.pdf
```

**SQL 的几条规则**

- 每次只能执行**一条**语句，且只能是读操作（`SELECT`/`WITH` 等），写操作和多条语句会被直接拒绝。
- 查询无法访问文件系统和网络（DuckDB 已关闭外部访问并锁定配置），所以不能用 SQL 把邮件导出去。
- 时间都是**本机本地时间**。`now()` 返回的是 UTC，`current_date` 不可用；按相对日期筛选时，
  先用 `--info` 看 `local_now`，再写成 `TIMESTAMP '2026-11-20'` 这样的字面量。
- 记得加 `LIMIT` 并只选需要的列，邮箱可能有几万封邮件。
- `ost_attachments` 需要指定邮件 nid，不能全库扫描；找带附件的邮件用 `has_attachments`。

### 6. 常见问题

| 现象 | 原因和处理 |
|---|---|
| `no .ost/.pst in any Outlook profile and no Mac Outlook profile found` | 没自动找到邮箱。先跑 `ost-mcp --list`；还是没有就把邮箱路径作为第一个参数传进去 |
| macOS 上 `--list` 为空或读取报权限错误 | 邮箱在其他 App 的数据目录里，macOS 可能弹窗询问是否允许访问，选允许。如果之前拒绝过，到“系统设置 → 隐私与安全性 → 完全磁盘访问权限”里给终端（或 Claude）打开权限，再重开终端 |
| macOS 提示“无法验证开发者”，拒绝运行 | 用浏览器下载的文件会带隔离标记，执行 `xattr -d com.apple.quarantine ~/.local/bin/ost-mcp` |
| 报 `unsupported` 且提到加密 | 这个 PST/OST 是加密的，目前只支持未加密的邮箱。Exchange 缓存模式的 OST 通常不加密，PST 常常加密 |
| `sender_name` / `sender_email` 为空 | 公司内部（Exchange）发件人在列表里拿不到名字和地址，属于已知限制。用 `--message <nid>` 看单封邮件时会补全 |
| Mac 上所有邮件都在 “Recovered Mail (Hx cache)” 里，`nid` 是负数，没有附件和收件人，正文只有约 250 字的预览 | 这是 New Outlook for Mac 本地缓存的限制，不是故障。按文件夹、附件筛选在这种邮箱上查不到结果 |
| 查询偶尔报格式错误 | Outlook 正在写入时可能读到写了一半的页，重试一次即可 |
| 配好 MCP 后 Claude 里看不到工具 | 确认配置里 `command` 是绝对路径，并且改完配置后重启了 Claude Code 会话或 Claude Desktop |

### 7. 安全提示

- 只读：OST/PST 以只读方式映射，`Outlook.sqlite` 以只读模式打开，没有任何写邮箱的代码。
  唯一会写磁盘的是 `--attachment ... --out`，而且只写你指定的那个文件。
- 接入后，模型能读到邮箱里的**所有**邮件，只在你信任的会话里使用。
- 邮件内容是发件人写的，可能包含诱导 AI 执行操作的文字。邮件里的任何“指令”都只是数据，
  不要让 AI 照着执行。
- 不要把邮件内容、主题、发件人地址或附件写进代码仓库、提交信息或 issue。

下面是英文原版说明，内容更完整（格式支持、已知限制等）。

## Why not export first

The usual approach converts a mailbox into an intermediate — an mbox tree, a
SQLite index, a folder of `.eml`. That copies every message you were trying to ask
one question about, goes stale the moment Outlook writes, and leaves a second
plaintext copy of your mail on disk. Mounting sidesteps all three, the way DuckDB
reads a Parquet file rather than loading it.

## Install

> **While this repository is private, the one-line installers below do not
> work:** an anonymous `curl`/`irm` gets a 404 for the script, the release
> assets and the skill. Until it is public, download the release with an
> authenticated `gh release download v0.2.0 -R tianxia--/ost-mcp`, or build
> [from source](#from-source) from a clone.

**Windows**, one line, in PowerShell:

```powershell
irm https://raw.githubusercontent.com/tianxia--/ost-mcp/main/install.ps1 | iex
```

That downloads the prebuilt binary from the latest release, checks it against its
published SHA-256, adds it to your PATH, drops the skill in
`~/.claude/skills/ost-mcp`, then opens your store and runs a query to show it works.
The Windows binary links the CRT statically, so **nothing else needs installing** —
no Rust, no MSVC, no Visual C++ redistributable.

Read it before you run it. Piping a remote script into a shell is a decision, not a
default:

```powershell
irm https://raw.githubusercontent.com/tianxia--/ost-mcp/main/install.ps1 | more
```

Options need a scriptblock rather than a pipe:

```powershell
& ([scriptblock]::Create((irm https://raw.githubusercontent.com/tianxia--/ost-mcp/main/install.ps1))) -FromSource
```

| Option | What it does |
|---|---|
| `-FromSource` | Builds with cargo instead of downloading. Needs Rust and the MSVC build tools |
| `-InstallPrereqs` | Installs a missing Rust toolchain or MSVC build tools with winget, for a source build |
| `-ReleaseTag <tag>` | Downloads a specific release instead of `latest` |
| `-SkillScope project` | Puts the skill in `.claude/skills` of `-ProjectPath` instead of your profile |
| `-SkipSkill` | Binary only |
| `-Force` | Reinstalls even when the same version is present, and ignores the MSVC check |
| `-Ref <branch>` | Builds from a branch other than `main`, which implies `-FromSource` |

**macOS**, one line, in Terminal:

```sh
curl -fsSL https://raw.githubusercontent.com/tianxia--/ost-mcp/main/install.sh | bash
```

That does the same thing: downloads the prebuilt binary, checks its SHA-256, puts it
in `~/.local/bin`, drops the skill in `~/.claude/skills/ost-mcp`, then mounts your
Outlook profile and runs a query to show it works.

Read it first the same way — `curl -fsSL <url> | less`. Options go after `-s --`:

```sh
curl -fsSL https://raw.githubusercontent.com/tianxia--/ost-mcp/main/install.sh | bash -s -- --from-source
```

| Option | What it does |
|---|---|
| `--from-source` | Builds with cargo instead of downloading. Needs Rust and Xcode Command Line Tools |
| `--install-prereqs` | Installs a missing Rust toolchain with rustup, and prompts for Xcode Command Line Tools, for a source build |
| `--release-tag <tag>` | Downloads a specific release instead of `latest` |
| `--skill-scope project` | Puts the skill in `.claude/skills` of `--project-path` instead of your home directory |
| `--skip-skill` | Binary only |
| `--force` | Reinstalls even when the same version is present, and continues past the Command Line Tools check |
| `--ref <branch>` | Builds from a branch other than `main`, which implies `--from-source` |

The published binaries are **Windows x64** and **macOS arm64**. On an Intel Mac, or
whenever a release asset is missing, both installers fall back to a source build on
their own — that path needs a toolchain and a few minutes to compile DuckDB. There is
no Linux build: Outlook does not run on Linux, so there is no store to read.

### Let an agent install it

Copy this to Claude Code, or any agent that can run a shell. It runs the installer,
fixes what fails, and checks the result.

```text
Install ost-mcp on this machine and confirm it works.

1. Run the line for this platform.
   Windows, in PowerShell:
     irm https://raw.githubusercontent.com/tianxia--/ost-mcp/main/install.ps1 | iex
   macOS, in a shell:
     curl -fsSL https://raw.githubusercontent.com/tianxia--/ost-mcp/main/install.sh | bash

2. It downloads a prebuilt binary, so it normally needs no compiler. If it falls
   back to a source build and then stops on a missing prerequisite, install what it
   names and run it again. To let it install the toolchain itself, add the flag for
   this platform:
     & ([scriptblock]::Create((irm https://raw.githubusercontent.com/tianxia--/ost-mcp/main/install.ps1))) -InstallPrereqs
     curl -fsSL https://raw.githubusercontent.com/tianxia--/ost-mcp/main/install.sh | bash -s -- --install-prereqs

3. Two failures worth knowing, both only on the source-build path. On Windows,
   LNK1104 on a library such as msvcrt.lib means cargo picked the wrong linker —
   re-run from a Developer PowerShell for VS 2022. If instead the download reports
   a SHA-256 mismatch, stop and tell me; do not work around it.

4. Check all four, and report the actual output of each:
   - `ost-mcp --version` prints a version
   - `ost-mcp --list` names at least one store
   - `ost-mcp --info` prints a backend kind and a folder count
   - the file ~/.claude/skills/ost-mcp/SKILL.md exists

5. Then tell me to restart my Claude Code session so the skill loads.

This tool reads my real mailbox, read-only. While you check the install, do not
print, quote or save a subject line, a sender address or any message body. Counts,
the backend kind and the folder count are the only evidence you need.
```

### From source

Needs a Rust toolchain and, on Windows, the MSVC build tools.

```sh
cargo build --release
```

The binary lands at `target/release/ost-mcp`. Use `cargo install --path
crates/ost-mcp` instead to put `ost-mcp` on your `PATH`, which is what the skill
expects.

## Use

```sh
ost-mcp                                # serve MCP over stdio on the discovered store
ost-mcp <file.ost>                     # serve MCP on a named OST/PST
ost-mcp <profile Data dir>             # serve MCP on a named Mac Outlook profile
ost-mcp [store] --list                 # list the stores that were discovered
ost-mcp [store] --info                 # path, backend kind, size, schema
ost-mcp [store] --sql "..."            # run one query, print JSON, exit
ost-mcp [store] --message <nid>        # one message: headers, recipients, body
ost-mcp [store] --attachments <nid>    # attachment metadata for a message
ost-mcp [store] --attachment <m>:<a>   # one payload, as text or base64
ost-mcp [store] --attachment <m>:<a> --out f   # write the payload to a file
ost-mcp --version                      # which build this is
```

Every MCP tool has a flag that prints the same JSON and exits, so the binary is
usable from a shell or a script without speaking the protocol. `--sql` covers
folder listing and search on its own, since both are queries over the same two
tables.

With no argument, the store is resolved from the Outlook profile registry on
Windows (`DefaultProfile` first) or from the Outlook group container on macOS —
rather than guessed from a filename or picked by directory scan. `--list` shows
which profile and account each store belongs to.

```sh
$ ost-mcp --sql "SELECT folder_path, count(*) FROM messages GROUP BY 1 ORDER BY 2 DESC LIMIT 3"
```

### As a Claude Code skill

Lower friction than an MCP server: no config file to edit and no restart. The
installer above does this for you; from a clone it is two commands.

```sh
cargo install --path crates/ost-mcp
cp -r skills/ost-mcp ~/.claude/skills/          # or .claude/skills/ in one project
```

The skill teaches the schema and the query patterns, then shells out to the flags
above. It costs nothing until a question actually needs the mailbox, where an MCP
server's tool definitions sit in the context of every turn. Set `OST_MCP_BIN` if
the binary is somewhere unusual. See [`skills/ost-mcp/SKILL.md`](skills/ost-mcp/SKILL.md).

### As an MCP server

```json
{
  "mcpServers": {
    "ost": {
      "command": "C:\\path\\to\\ost-mcp.exe"
    }
  }
}
```

Add the store path as an argument if you want a specific one:
`"args": ["C:\\path\\to\\archive.pst"]`.

## Tools

| Tool | What it does | CLI |
|---|---|---|
| `store_info` | Path, backend kind, folder count, schema | `--info` |
| `list_folders` | The folder tree with item and unread counts | `--sql` |
| `search` | Messages by text, folder, date range, attachment presence | `--sql` |
| `sql` | One read-only statement, for anything `search` cannot express | `--sql` |
| `get_message` | One message: headers, recipients, body, attachment list | `--message` |
| `list_attachments` | Attachment metadata without reading a payload | `--attachments` |
| `read_attachment` | One attachment's bytes, as text when they are text | `--attachment` |

Both surfaces build their replies from the same functions, so a tool and its flag
return identical JSON.

`sql` runs against a DuckDB with external access disabled, so a query can read the
mailbox but cannot reach the filesystem or the network.

## Schema

```sql
folders(nid, parent_nid, name, path, item_count, unread_count,
        has_subfolders, is_search_folder)

messages(nid, folder_nid, folder_path, subject, sender_name, sender_email,
         delivered, submitted, modified, size, unread, has_attachments,
         message_class)

ost_attachments(message_nid => <nid>)
  -> (message_nid, nid, filename, mime, content_id, declared_size, data_len)
```

`nid` is the handle for everything — a message's comes from a query, an
attachment's from `list_attachments`.

## Format support

The reader implements MS-PST's three layers (NDB, LTP, messaging) plus the
**undocumented format version 36** that Outlook 2013 and later write for cached
Exchange mailboxes: 4 KB pages, widened BTree page counts, a moved heap-id bit
split, and zlib-compressed block payloads with no flag announcing them. That work
is written up in [`docs/ost-v36-format.md`](docs/ost-v36-format.md), which is
probably more useful than this reader if you are implementing your own.

Known limits:

- **Encrypted stores are rejected**, cleanly. Only `NDB_CRYPT_NONE` is
  implemented; `NDB_CRYPT_PERMUTE` and `NDB_CRYPT_CYCLIC` need a substitution
  table from the spec that has not been added. Cached Exchange OSTs are typically
  unencrypted; PSTs often are not.
- **Version 23 is implemented but untested** against a real PST. Versions 14 and
  15 (Outlook 2002 and earlier) are rejected.
- **Bulk `sender_name` and `sender_email` are NULL for internal Exchange senders.**
  A contents table has no usable sender-name column, so those come from the sender
  EntryID, which spells out a name and address only for internet senders.
  `get_message` resolves the rest.
- **Named properties (0x8000 and above) are not resolved** — the named-property
  map is not parsed yet.
- **Torn pages are not retried.** Outlook writes while you read and every page
  carries a CRC, so this is detectable, but validation is not wired up.

**Mac Outlook** (`Outlook.sqlite` + `.olk15*` + `HxStore.hxd`,
`docs/mac-outlook-format.md`):

- **Two local stores, read together.** `Outlook.sqlite` + `.olk15*` (the
  classic engine) supplies folder, category and signature structure.
  `HxStore.hxd` — New Outlook's undocumented local cache, independently
  parsed here — supplies message content for whatever window the account's
  own sync setting keeps locally (e.g. the last 60 days), for at least
  Exchange/M365 accounts. Either can be empty on a given account without
  the other being; both return nothing rather than a guess when they are.
- **Recovered mail has no folder identity.** `HxStore.hxd` does not tie a
  message to a specific folder, so every message it supplies is exposed
  under one synthetic "Recovered Mail (Hx cache)" folder, not sorted into
  Inbox/Sent/etc.
- **Recovered mail has no attachment linkage.** A separate plain-file
  attachment cache exists (`Files/S0/<n>/Attachments/0/*`), but nothing
  found so far ties one of its files to a specific message.
- **Recovered mail's sender is sometimes NULL rather than a guess.** When a
  record's sender metadata sits after the anchor and holds more than one
  address (a participant's and the true sender's), there is no reliable way
  yet to tell them apart — see `docs/mac-outlook-format.md` §2.6.1.
- **Classic-engine message bodies come from `.olk15Message` only.**
  `.olk15MsgSource` (the higher-fidelity, full-RFC822 file some messages
  get) is located but not parsed yet — that needs a real MIME parser.
- **Classic-engine recipients are not populated.** `Mail` carries flat
  recipient-address-list columns, not a structured table, and the
  delimiter/encoding has not been measured against a real row.
- **Classic-engine folder names for the standard special folders (Inbox,
  Sent Items, …) are inferred from a measured type code, not read
  verbatim** — Outlook itself writes a literal placeholder string into
  `Folder_Name` on an account whose classic engine holds no real folder
  data, and that string is never surfaced.

## Safety

Read-only by construction: an OST/PST is memory-mapped without write access, a Mac
profile's `Outlook.sqlite` is opened with SQLite's read-only flag, and neither
backend has a code path that writes to a store. `--attachment --out` is the only
thing that writes anywhere, and it writes one payload to the path you name.

It is still your mail — a model with this server or skill attached can read every
message in the mailbox, so attach it deliberately.

## License

MIT. See [LICENSE](LICENSE).
