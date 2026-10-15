# Sync, backup and build/distribution research: Rust + Qt 6 creative suite on Windows 11

*Research date: 2026-10-12. Sources are primary: vendor pricing pages, official docs, the GitHub API for repo and release status, and the AWS Price List API. Anything marked **[UNVERIFIED]** couldn't be confirmed from a primary source during this session. Prices are list prices in USD unless shown in EUR, excluding tax.*

---

## TL;DR

| Decision | Recommendation |
|---|---|
| Code and metadata hosting | GitHub Free, private repos. Don't store large binaries in GitHub LFS: it costs about $0.07/GiB-month plus $0.0875/GiB download and has a 2 GB per-file cap on Free/Pro. |
| Large binaries | **Git LFS with a standalone custom transfer agent** (`lfs-s3`, Go, Windows `.exe` provided, released 2026-04) pointed at a **Backblaze B2** bucket. Cloudflare R2 is the alternative if zero egress matters. No server to run. The suite's workspace-init command writes the per-clone `git config`. |
| Backup tool | **restic 0.19.1**. It has native Windows support, VSS (`--use-fs-snapshot`), the most complete `--json` output plus documented exit codes (best for driving from the suite), and `restic copy` between repos. Kopia 0.23.1 is the runner-up if you want a GUI and built-in scheduler. |
| Backup targets | Repo 1 on the external drive. Repo 2 at a *different provider* from the asset bucket, for example a Hetzner Storage Box over SFTP or a second B2 account/bucket with Object Lock. |
| Installer and updates | **Velopack** (Rust crate, per-user `%LocalAppData%` install, no admin, delta updates, built-in Artifact Signing support), fed by a `windeployqt` staging folder. Use Inno Setup 7 or WiX 7 only if you need a classic installer. |
| Code signing | For your own machines it's optional, but **Windows 11 Smart App Control blocks unsigned executables outright** when it's on. Azure Artifact Signing (formerly Trusted Signing) costs $9.99/month. Individuals are eligible only in the USA and Canada. |
| Cross-platform | Defer it. Keep all D3D and Win32 code behind traits. Linux via Vulkan/OpenGL QRhi is the cheaper second target. |
| Estimated monthly cloud cost (assets + backup) | **200 GB:** about $1.30 + €3.20. **1 TB:** about $6.90 + €10.90. **3 TB:** about $20.80 + €10.90. These use the recommended B2 + Hetzner Storage Box setup. |

---

## 1. GitHub: current limits and pricing for private repos

### 1.1 Git LFS

| Item | Free | Pro | Source |
|---|---|---|---|
| Included LFS storage | 10 GiB | 10 GiB | GitHub Docs, LFS billing |
| Included LFS bandwidth (download only) | 10 GiB/month | 10 GiB/month | same |
| Overage storage | $0.07 per GiB-month, metered hourly | same | GitHub pricing calculator |
| Overage bandwidth | $0.0875 per GiB downloaded | same | GitHub pricing calculator |
| Data packs | **Removed**. Billing is now metered. | | GitHub Docs: "pre-paid data packs have been removed" |
| Default budget | $0 if there's no payment method, so usage is blocked once the quota is exceeded | | GitHub Docs (budget behaviour) |
| Max LFS file size | **2 GB** | **2 GB** | Team: 4 GB. Enterprise Cloud: 5 GB. |
| Uploads | Count toward storage, not bandwidth | | GitHub Docs |

Notes:
- Team and Enterprise get 250 GiB storage and 250 GiB bandwidth included. Free and Pro don't.
- The pricing calculator still says LFS "will switch to metered billing", while the billing docs say data packs are already removed. The two pages are inconsistent, but both give the same per-GiB rates.
- A 2025 community reply mentions a $5 per 50 GB pack. That isn't on any current primary page, so treat it as obsolete **[UNVERIFIED]**.

### 1.2 Repository and file size limits

| Limit | Value |
|---|---|
| Git warning | >50 MiB per file |
| Git hard block | >100 MiB per file |
| Browser upload | 25 MiB |
| Recommended repo size | <1 GB ideal, <5 GB strongly recommended. The "repository limits" page gives a 10 GB recommended max for on-disk `.git`. |
| Single push | 2 GB (enforced) |
| Directory width | 3,000 entries per directory |
| Directory depth | 50 levels |
| Push rate | 6 pushes per minute per repo |

### 1.3 GitHub Actions (private repos)

| Plan | Included minutes/month | Artifact + Packages storage | Cache per repo |
|---|---|---|---|
| Free | 2,000 | 500 MB | 10 GB |
| Pro | 3,000 | 1 GB | 10 GB |

Per-minute overage rates (post-January-2026 price cut, from the "actions-runner-pricing" reference):

| Runner | $/min |
|---|---|
| Linux 2-core x64 | $0.006 |
| Linux 1-core (slim) | $0.002 |
| **Windows 2-core x64** | **$0.010** |
| Windows 2-core arm64 | $0.010 |
| macOS 3/4-core | $0.062 |

Other storage overage: artifacts and Packages cost $0.25/GB-month. Cache costs $0.07/GB-month.

- **Windows multiplier.** The current docs no longer describe the older "Windows 2×, macOS 10×" minute multipliers. They list per-minute dollar rates instead (Windows at $0.010 is about 1.67× Linux). Their worked example simply adds Linux and Windows minutes together. The docs don't state clearly whether Windows minutes draw down the included quota at 1× or at a dollar-equivalent rate **[UNVERIFIED]**. To be safe, budget roughly 1,000–1,200 effective Windows minutes per month on Free.
- **Larger runners** are always billed, even when quota remains.
- **Self-hosted runner fee.** In December 2025 GitHub announced a $0.002/min "cloud platform" charge for self-hosted runners in private repos, starting 2026-03-01. It **postponed** that charge to "re-evaluate our approach". The current billing docs say self-hosted usage is free, and no new date had been announced as of this research.

### 1.4 Artifacts and Release assets

| Item | Limit |
|---|---|
| Release asset size | each file < **2 GiB** |
| Assets per release | up to 1,000 |
| Total release size and bandwidth | "We don't limit the total size … or the bandwidth" |
| Actions artifacts | count against the 500 MB / 1 GB plan storage, shared with Packages |

Release assets are a good home for installers and Velopack update feeds. They aren't a good home for workspace assets.

### 1.5 Self-hosted runners on Windows
- Supported: Windows 10 and 11 64-bit; Windows Server 2016, 2019 and 2022 (x64). ARM64 is in public preview.
- Free, per the postponement above.
- They're useful for Qt + Rust builds, since the Qt SDK and cargo cache are already on your box. If the runner fails a critical update, GitHub stops queueing jobs to it.
- Security practice: only attach a self-hosted runner to private repos you control **[general guidance, not quoted from that page]**.

---

## 2. Keeping large binaries outside GitHub while git tracks them

### 2.1 The two git-lfs hooks you can use

1. **`.lfsconfig` with `lfs.url` / `remote.<name>.lfsurl`.** This points the LFS API at a server other than GitHub while the git repo stays on GitHub. `.lfsconfig` is committed, so it travels with clones. Only a whitelisted set of keys is honoured there: `lfs.url`, `lfs.pushurl`, `remote.{name}.lfsurl`, `lfs.{*}.access`, `lfs.fetchinclude`, `lfs.fetchexclude`, `lfs.locksverify` and a few others. The docs say the set is "restricted for security reasons". Local git config overrides `.lfsconfig`.
2. **Standalone custom transfer agent** (`lfs.standalonetransferagent` + `lfs.customtransfer.<name>.path/args`). With this, git-lfs **never contacts an LFS API server** and hands each object directly to your agent. These keys are **not** allowed in `.lfsconfig`, so they must be set per clone in `.git/config` or globally. git-lfs also ships a built-in `lfs-standalone-file` adapter for `file:///` URLs, which suits a NAS share. Whether UNC paths work with it is **[UNVERIFIED]**.

Practical pattern from the lfs-dal README:
- Commit `.lfsconfig` with a dummy `lfs.url`, so a clone that hasn't been configured can never silently push to GitHub LFS.
- Clone with `GIT_LFS_SKIP_SMUDGE=1`, configure the agent, then run `git lfs pull`.

The suite's "open/clone workspace" command can do all of this automatically.

### 2.2 Options compared

Status data comes from the GitHub API on 2026-10-12.

| Option | Type | Lang | Latest release / last push | Backends | Auth | Windows | Verdict |
|---|---|---|---|---|---|---|---|
| **lfs-s3** (nicolas-graves) | Standalone transfer agent, no server | Go | 0.2.2 (2026-04-21) / 2026-05 | Any S3 endpoint (`--endpoint`, path-style option), zstd compression | S3 keys in git config or env/AWS profile | `lfs-s3-windows.exe` in releases | **Best fit.** Caveats: tested only with git-lfs 3.3–3.4; keys sit in cleartext in `.git/config`, so prefer `AWS_PROFILE`; no client-side encryption. R2/B2 aren't named but should work as generic S3 endpoints **[UNVERIFIED with R2/B2]**. |
| **lfs-dal** (regen100) | Standalone transfer agent via Apache OpenDAL | Rust | v0.1.3 (2024-09-16) | S3, Azure, GCS, WebDAV, OneDrive, Google Drive, Dropbox… | Per-service, `.lfsdalconfig` | `x86_64-pc-windows-msvc` zip | Good design, but dormant since 2024. Still a reasonable fallback, or fork it since it's Rust. |
| **Rudolfs** (jasonwhite) | LFS API server with S3 backend and local disk cache | Rust | tag 0.3.8; commits 2026-09-15 | S3 or local disk | **None.** Its README says there's no client auth and it's meant for internal networks. | Docker image (<10 MB) | Good on a NAS or LAN (or behind Tailscale) with optional xchacha20 encryption at rest. Needs an always-on host. |
| **Giftless** (datopian) | Pluggable LFS server | Python | v0.6.2 (2024-12-16) / 2025-12 | Local, S3, GCS, Azure | JWT / pluggable | Python / WSGI | Too heavy for one user. Maintenance is slow. |
| **LFS Test Server** (git-lfs) | Reference server | Go | v0.4.0 (2018) | Local disk | Basic | Binaries | README: "not in a production ready state". **Avoid.** |
| git-lfs-s3-proxy (now twilligon/) | Cloudflare Worker that presigns R2/S3 URLs | JS | no releases / 2025-11 | S3 / R2 | creds in URL | n/a (serverless) | Interesting for R2. Low activity. |
| git-lfsaver | Workers + R2 with GitHub auth | | v1.0.2 (2026-10-07), 1 star | R2 | GitHub | n/a | Too new to trust. |
| lfs-folderstore | Agent for a folder or NAS | Go | **archived** 2023 | folder | | | Use git-lfs's built-in `lfs-standalone-file` instead. |
| **git-annex** | Separate large-file system | Haskell | | many special remotes | | Windows port is "in reasonably good shape for command-line use", with known problems: no SSH connection caching, so it's slow per file; gcrypt not ported; filename and encoding issues; symlink caveats | Powerful, but **not robust on native Windows** for this use. |
| **DVC** (now stewarded by lakeFS/Treeverse since 2025-11-18) | `.dvc` pointer files plus a cache, separate CLI | Python | 3.67.1 (2026-03-31) | S3-compatible via `endpointurl`, plus many others | `--local` credentials | Works on Windows (pip/installer) **[install page not fetched]** | A good fit for **model weights and datasets**. It isn't transparent to git (`dvc push/pull`). Long-term roadmap after the acquisition is unclear. |
| GitHub LFS itself | Hosted | | | GitHub | GitHub | Ships with Git for Windows | Simplest option, but about $64/month at 1 TB and a 2 GB file cap. |

### 2.3 What's robust on Windows for one user

1. **Recommended:** git-lfs (bundled with Git for Windows, v3.8.0 released 2026-08-28) plus the `lfs-s3` standalone agent, writing to a B2 or R2 bucket.
   - Nothing to host.
   - Objects are content-addressed by SHA-256, so they're immutable and dedupe naturally.
   - Git tracks pointers, and GitHub only ever sees pointers.
   - The suite writes `lfs.standalonetransferagent`, `lfs.customtransfer.lfs-s3.path` and `args` into each clone and keeps credentials in an AWS profile or Windows Credential Manager.
2. **If you get a NAS:** use git-lfs's built-in `lfs-standalone-file` with a `file:///` URL on the share, or Rudolfs in Docker on the NAS. Either way the NAS isn't offsite, so the cloud backup is still required.
3. **Model weights** (huge, rarely change, re-downloadable): keep them **out of git entirely**. A manifest records hash, size and source URL, and the suite fetches them on demand. DVC is an option if you want versioned pointers.
4. Avoid git-annex on native Windows, LFS Test Server, and anything that requires a public server without auth (Rudolfs on the open internet).

> Note: the LFS bucket is *storage*, not a *backup*. Credential misuse, `git lfs prune` on the wrong machine, or provider lock-out can lose it. Keep the separate encrypted backup (section 4).

---

## 3. S3-compatible cloud storage pricing (as of 2026-10)

| Provider | Storage | Egress | API ops | Min. storage duration / min. charge | Free tier | Notes |
|---|---|---|---|---|---|---|
| **Backblaze B2** (pay-as-you-go) | **$6.95/TB-mo** | Free up to **3× avg stored** per month, then $0.01/GB. Free via CDN partners. | Class A, B, C free. Class D $0.004/10k after 2,500/day. | **None** | 10 GB | S3-compatible API; restic recommends B2's S3 API over its native B2 backend. Object Lock exists **[not re-verified this session]**. |
| **Cloudflare R2** Standard | **$0.015/GB-mo** ($15/TB) | **Free** | Class A $4.50/M, Class B $0.36/M | None | 10 GB-mo, 1M A, 10M B | |
| Cloudflare R2 Infrequent Access | $0.01/GB-mo | Free egress, but **$0.01/GB retrieval** | A $9/M, B $0.90/M | **30 days** | none | |
| **Wasabi** (pay-as-you-go) | **$7.99/TB-mo** | Free if monthly egress ≤ active storage. "Not a good fit" above that. | Free | **90 days** (timed deleted storage) and a **1 TB minimum** monthly charge | — | |
| **AWS S3 Standard** (us-east-1) | **$0.023/GB-mo** first 50 TB ($23/TB) | $0.09/GB first 10 TB/mo, after 100 GB/mo free (aggregate) | PUT $0.005/1k, GET $0.0004/1k | none | 100 GB egress/mo | Price List API, publication 2026-09-28 |
| AWS S3 **Glacier Instant Retrieval** | **$0.004/GB-mo** ($4/TB) | $0.09/GB plus **$0.03/GB retrieval** | PUT $0.02/1k, GET $0.01/1k | **90 days**; 128 KB minimum object | | Restoring 1 TB costs about $30 retrieval + about $81 egress. |
| **iDrive e2** | **$6/TB-mo** (pay-as-you-go) | Free up to 3× active storage, then $0.01/GB | Free | No minimum duration, but a **1 TB minimum charge** | 10 GB trial | Annual 1 TB plan: $59.50/yr ($29.75 first year). |
| **Hetzner Object Storage** | **€6.49/mo base** (USD shown $7.99), includes 1 TB storage + 1 TB egress. Additional storage €6.26/TB-mo (USD shown $8.86). | Additional egress €1.00/TB (USD $1.20). Ingress free. | Free | None (hourly billing with monthly cap); 64 KB minimum object | — | EU only (FSN1, NBG1, HEL1). Prices were taken from the live page's embedded data, with the VAT selector at 0%. Hetzner raised Object Storage prices in April 2026 (secondary sources say from €4.99). |
| **Hetzner Storage Box** (not S3: SFTP, SMB, WebDAV, rsync, **restic/borg/rclone**) | **BX11 1 TB €3.20**, **BX21 5 TB €10.90**, BX31 10 TB €20.80, BX41 20 TB €40.60 per month; no setup fee | **Unlimited traffic** | n/a | No minimum contract; cancel immediately | — | Snapshots included. Prices were read from the live page with VAT set to 0% (non-EU), so EU customers add VAT. These may be post-2026-adjustment prices **[treat as current list, check before ordering]**. |
| **Google One / Google AI plans** (consumer) | 100 GB $1.99/mo; **2 TB $9.99/mo** ("Google AI Plus"); **5 TB $19.99/mo** ("Google AI Pro") | n/a | Drive API limits | — | 15 GB | Max file 5 TB. A 750 GB/day upload cap is documented for Workspace (assume the same for consumer **[UNVERIFIED for consumer]**). Not S3; usable via rclone. |
| **OneDrive** (consumer) | M365 Personal **1 TB $99.99/yr**; Family **2 TB shared $129.99/yr**; Premium $199.99/yr (Microsoft pages describe up to 6 TB, 1 TB per person) | n/a | Graph API throttling | — | 5 GB **[UNVERIFIED]** | Max file 250 GB. Sync performance is best below 300k items. No path to more than 1 TB for a single person. Not S3. |

**Takeaways.** For one user, B2 has the cheapest per-TB storage with enough free egress (3×). R2 costs about 2× on storage but has zero egress and no egress caps. The 1 TB minimum charge on Wasabi and iDrive e2 makes them poor value at 200 GB. Glacier IR is cheap to store but expensive to restore. Hetzner Storage Box is the cheapest backup target at 1–5 TB, using restic over SFTP. Consumer drives are fine for documents and poor for backup repositories: API throttling, rclone-only access and file count limits.

---

## 4. Backup tools

| | **restic** | **Kopia** | **Borg** | **Duplicacy** |
|---|---|---|---|---|
| Latest (GitHub) | **v0.19.1** (2026-07-05); repo active daily | **v0.23.1** (2026-06-16); active daily | **1.4.5** stable (2026-07-18); **2.0.0b25** beta (2026-09-27) | CLI **v3.2.5** (2025-05-03); repo pushed 2026-08 |
| License | BSD-2 | Apache-2.0 | BSD-3 | Custom: CLI free for personal use; GUI $20 first computer, then $5/yr |
| Native Windows | **Yes** | **Yes** (CLI + KopiaUI) | **No.** 1.4 docs say WSL/Cygwin is "experimental". The master (2.x) docs add an MSYS2 build: "Running under Windows is experimental." | Yes |
| Encryption | Always on, client-side **[algorithm details not re-fetched]** | Always on (AES-256 or ChaCha20; unencrypted not supported) | Yes (repokey/keyfile) | Optional `-e` **[details not re-fetched]** |
| Dedup / compression | Content-defined chunking; compression in repo v2 | CDC, rolling hash; pgzip, s2, zstd; Reed-Solomon ECC option | CDC; lz4/zstd | Lock-free CDC |
| **Windows VSS** | **Yes:** `--use-fs-snapshot`, with `-o vss.timeout`, `vss.exclude-volumes`, `vss.provider`. Needs admin **[UNVERIFIED, implied by VSS]** | **Yes, but experimental since 0.16.1:** policy `--enable-volume-shadow-copy=never/always/when-available`. Open issues: drive root, junctions, needs admin (issue #3798). | No | Yes: `-vss` (Windows, macOS APFS) |
| Scheduling | None built in; use Task Scheduler | **Built in** via policies (`--snapshot-interval`, `--snapshot-time`) while KopiaUI or the server runs | None (borgmatic is Linux-focused) | Web GUI scheduler (paid); CLI uses Task Scheduler |
| S3 / B2 / R2 | S3 and S3-compatible (use B2 via its S3 API), native B2, Azure, GCS, SFTP, REST server, rclone, local | S3 and compatible, B2, Azure, GCS, WebDAV, SFTP, rclone, local, Kopia server | 1.x: local and SSH only (Hetzner Storage Box supports it). 2.x borgstore: sftp, REST, **S3/B2 native**, rclone. | S3, B2, SFTP, Azure, GCS, Wasabi, OneDrive, Google Drive… **[backend list not re-fetched]** |
| **Machine-readable output** | **Best.** `--json` on backup, restore, check, ls, snapshots, stats, diff, find, forget, init, key list, tag, version. Status, summary and error message types. Documented exit codes: 0, 1, 3 (partial read), 10 (no repo), 11 (lock failed), 12 (wrong password), 130 (cancelled). | `--json`, `--json-indent`, `--json-verbose` on `snapshot create`, `snapshot list` and others | `--json` on create, list, info **[not re-fetched]** | Not documented |
| Restore UX on Windows | `restic restore` (CLI). **`restic mount` is not available on Windows** (FUSE only on Linux, macOS, FreeBSD). Third-party GUIs exist (Backrest **[UNVERIFIED]**). | **KopiaUI** browse and restore; `kopia mount all Z:` maps a drive letter via WebDAV (needs the WebClient service) | `borg mount` via WinFsp only in experimental 2.x builds | GUI restore (paid) or CLI |
| Repo-to-repo copy | `restic copy` | `kopia repository sync-to` | `borg transfer` (2.x) | `duplicacy copy` |

### Best fit for one user, Windows, external drive + B2/R2, large media, driven by the suite

**restic.**
- Its JSON-lines progress, summary output and stable exit codes make it the easiest to wrap from Rust. Spawn it as a process and parse `message_type`.
- VSS is first-class and not experimental.
- It's a single static `.exe` that the suite can ship or locate.
- Its repo format is stable and well documented.

Layout:
- `restic -r E:\backup\worldwright backup --use-fs-snapshot --json ...` runs nightly via Task Scheduler. The suite can register the task.
- `restic copy` (or a second `backup` run) sends to the cloud repo. Use B2 through its S3 endpoint, or SFTP to a Hetzner Storage Box.
- Run `forget --prune` and periodic `check --read-data-subset` as maintenance.
- Exclude large media that's already in the LFS bucket only if you accept relying on the bucket for those files. The user decided on full backups, so the cost table below assumes they're included.

**Kopia** is the pick if you'd rather have a GUI, built-in scheduling and a WebDAV drive-letter restore. Its JSON support is good too. The caveat is that VSS is still flagged experimental and has open Windows issues.

**Borg:** no for native Windows. **Duplicacy:** viable, but it has no JSON output, a paid GUI, and the last CLI release was in 2025.

---

## 5. Windows installers for a Rust + Qt 6 app

### 5.1 Packaging and installer tools

| Tool | Latest (2026-10) | Format | Per-user without admin | Notes for Rust + Qt |
|---|---|---|---|---|
| **WiX Toolset** | **v7.0.0** (2026-04-06). v5/v6 are superseded. | MSI / Burn bundles | Yes. `Package/@Scope` takes per-machine (default), per-user, or "a choice of either". | Full control and MSI semantics, but a steep learning curve. The **Open Source Maintenance Fee** requires organizations with more than $10k annual revenue to sponsor; a hobby project is below that threshold (the page doesn't explicitly say "exempt"). Use `heat`-style harvesting or a generated `.wxs` for the windeployqt folder. |
| **MSIX** | OS feature | `.msix` | **Yes.** Packaged apps install per-user by design. | **Must be signed** by a cert that chains to a trusted root. Self-signed works only if you install the cert on each machine. App Installer handles auto-update. Packaging a Win32 Qt app works via a full-trust manifest **[details not re-fetched]**. The Microsoft Store re-signs MSIX for free. |
| **Inno Setup** | **7.1.0** (2026-08-12) | `.exe` | Yes. `PrivilegesRequired=lowest` never requests admin. | Easiest scripting. Free for non-commercial use; a commercial license is "requested" above $5k revenue. |
| **NSIS** | **3.13** (2026-09-27) | `.exe` | Yes (`RequestExecutionLevel user`) **[not re-fetched]** | Mature but script-heavy. Used by cargo-packager. |
| **cargo-dist** (axodotdev) | v0.33.0 (2026-09-11) | MSI, PowerShell, shell, npm, Homebrew, updater | MSI defaults to `%ProgramFiles%` | Uses **WiX v3** (v4+ not supported). Only includes "the app's binaries in a `bin` subdir", so it's a **poor fit for Qt DLL and plugin trees** unless you hand-edit `main.wxs`. Its CI/release orchestration is good. |
| **cargo-packager** (CrabNebula) | 0.11.8 (2025-11-27); repo active | NSIS `.exe`, WiX `.msi` | **[not stated in docs fetched]** | Has a `cargo-packager-updater` crate. Resources config exists **[not verified for Qt trees]**. |
| **Velopack** | **1.2.161** (2026-09-29), Rust crate `velopack` | `Setup.exe` (+ optional `--msi`) | **Yes.** `Setup.exe` installs to `%LocalAppData%\{packId}`. MSI `PerUser` needs no elevation; `PerMachine` needs elevation. | Packs any folder (`vpk pack -p <dir> -e app.exe`), so it **fits a windeployqt output directly**. Zstd **delta updates**. Updates hosted as static files (GitHub Releases, bucket, HTTP). Signing via `--signParams` (signtool), `--signTemplate`, or **`--azureTrustedSignFile`**. Note: `vpk` needs the .NET 8 SDK on the build machine. |

### 5.2 Auto-update options

| Option | How it works | Fit |
|---|---|---|
| **Velopack** | `VelopackApp::build().run()` first in `main()`, then `UpdateManager` check, download, and `apply_updates_and_restart()`. Delta packages. | **Best for this stack.** Per-user, no admin, Rust-native, and the feed can live on GitHub Releases or in the B2/R2 bucket. |
| MSIX App Installer | An `.appinstaller` file gives on-launch or background checks, `HoursBetweenUpdateChecks`, `ShowPrompt`, `UpdateBlocksActivation`, and fallback `UpdateURI`s. Windows 10 2004+ and 11. | Good if you go MSIX, but it **requires trusted signing**. |
| **WinSparkle** v0.9.4 (2026-07-21) | Sparkle-compatible appcast. **EdDSA (Ed25519) signed** enclosures (DSA deprecated). C API (bind via FFI). MIT. | Fine with an Inno, NSIS or MSI installer as the update payload. More plumbing than Velopack. |

### 5.3 Code signing

- **Is it needed for personal use?** Not strictly, but be aware of two behaviours. Both come from Microsoft Learn.
  - **SmartScreen.** An unsigned file that has Mark-of-the-Web (downloaded via a browser) shows "Windows protected your PC", and the user must click **Run anyway**. Unsigned files start at zero reputation on every new version. A **self-signed** cert behaves the same as no signature.
  - **Smart App Control (Windows 11).** "If the app is unsigned, or the signature is invalid, Smart App Control will consider it untrusted and block it." Its checks apply to **all executables, not just downloads**. Recent Windows updates allow it to be re-enabled without a clean install. On any machine where it's on, an unsigned suite simply won't run. On your dev box you'll likely have it off.
  - EV certificates **no longer bypass SmartScreen** (since 2024).
- **Azure Artifact Signing** (renamed from Trusted Signing; pricing unchanged):

| SKU | Price | Signatures/month | Overage | Certificate profiles |
|---|---|---|---|---|
| Basic | **$9.99/month** per account | 5,000 | $0.005 each | 1 of each type |
| Premium | $99.99/month | 100,000 | $0.005 each | 10 of each type |

  - **Eligibility (Public Trust):** organizations in the USA, Canada, EU, UK, Australia, New Zealand, Japan, South Korea, Singapore, Switzerland, Norway and Israel. Organizations need about **3+ years of verifiable tax history**. **Individual developers: USA and Canada only.** Individual validation uses the Azure billing account (type *Individual*) plus a government ID via AU10TIX and Microsoft Authenticator Verified ID.
  - Requirements: a paid Azure subscription (free, trial and sponsored subscriptions aren't supported). Billing isn't prorated.
  - Certificates are short-lived (issued daily, valid about 3 days), so always **timestamp** your signatures.
  - It doesn't give instant SmartScreen trust. Reputation builds across releases that share the same identity.
  - SignTool needs the Artifact Signing Client Tools (`winget install -e --id Microsoft.Azure.ArtifactSigningClientTools`). There's a GitHub Action, and Velopack has native support.
- Alternatives:
  - OV certificate: $150–300/yr per Microsoft's comparison, with an HSM or token required.
  - Microsoft Store MSIX: free re-signing and no SmartScreen warnings.
  - SignPath Foundation: free, but for OSS only.
- **Recommendation:**
  - Start unsigned for your own machines (Smart App Control off).
  - If you're in the US or Canada and start installing on machines you don't fully control, turn on Artifact Signing Basic ($120/yr) and sign both the exe/DLLs and `Setup.exe`.
  - Otherwise, consider the Store (MSIX) route.

### 5.4 windeployqt integration
- Build the release exe, then run `windeployqt --release --dir <stage> <exe>`.
  - Add `--qmldir` if you use QML. Add `--no-translations` if you don't need them.
  - It copies the Qt DLLs, plugins (with `platforms\qwindows.dll` in a `platforms` subfolder) and QML modules.
- Then add **third-party DLLs yourself**. windeployqt doesn't handle non-Qt libraries.
- **VC++ runtime (corrected 2026-10-12 by review):** an earlier version of this note over-read Microsoft's warning about loose DLLs. Microsoft permits **app-local deployment** of the VS `Redist\MSVC\v14x\x64\Microsoft.VC14x.CRT` DLLs (not recommended "for servicing reasons"). The spec ships them app-local so installation never needs admin; the redist installer would need UAC.
- Pipeline: `cargo build --release` → `windeployqt --release --dir stage` → copy extra DLLs → sign every PE in `stage` → `vpk pack -p stage -e suite.exe` (signing again via vpk) → upload `Releases/` to GitHub Releases or the bucket.

---

## 6. Cross-platform (brief)

- **Possible later, not worth designing for now beyond keeping boundaries clean.**
- **QRhi** supports Vulkan, Metal (macOS/iOS), OpenGL/ES, D3D11/12 (Windows only) and Null. Shaders written once compile to SPIR-V and are packed into `.qsb` via Qt Shader Tools, so code that stays on the QRhi abstraction ports well.
- **What would block a port:**
  1. Anything that reaches through `nativeHandles()` to D3D11/D3D12 devices, DXGI shared handles, or D3D-CUDA interop.
  2. Win32-specific APIs: VSS, registry, Credential Manager, Media Foundation or WIC codecs, DirectWrite, named pipes, file-locking semantics.
  3. Windows-only installer and updater choices (MSIX, WiX). Velopack and Qt are cross-platform.
  4. QRhi has **no source or binary compatibility guarantee** across Qt minor versions, so pin Qt.
  5. macOS adds notarization and an Apple Developer account **[cost not verified]**, plus $0.062/min macOS CI runners.
- **Linux** (Vulkan/OpenGL QRhi, AppImage or Flatpak) is the cheaper second target.
- Keep platform code behind Rust traits (`GpuInterop`, `SnapshotProvider`, `SecretStore`).

---

## 7. Cost estimates: assets + backup (monthly, list price, ex-VAT)

**Assumptions:**
- Primary asset store holds 1.0× the asset size, and the cloud backup holds another 1.0×. Media doesn't compress, and restic dedup only saves on duplicates. Add 20–50% for LFS version history and backup retention.
- 1 TB = 1,000 GB; GitHub bills per GiB.
- The external drive is a one-time hardware cost, not included **[price not researched]**.
- GitHub Free costs $0. CI fits within free minutes.

### 7.1 Per-provider storage cost (one copy)

| Provider | 200 GB | 1 TB | 3 TB | Full 1 TB restore (egress) |
|---|---|---|---|---|
| Backblaze B2 | $1.32 | $6.88 | $20.78 | $0 (≤3× stored) |
| Cloudflare R2 Standard | $2.85 | $14.85 | $44.85 | $0 |
| Cloudflare R2 IA | $2.00 | $10.00 | $30.00 | ~$10 retrieval |
| Wasabi | $7.99 (1 TB min) | $7.99 | $23.97 | $0 (≤ stored) |
| iDrive e2 | $6.00 (1 TB min) | $6.00 | $18.00 | $0 (≤3×) |
| AWS S3 Standard | $4.60 | $23.00 | $69.00 | ~$81 |
| AWS S3 Glacier IR | $0.80 | $4.00 | $12.00 | ~$111 (retrieval + egress), 90-day min |
| Hetzner Object Storage | €6.49 | €6.49 | €19.01 | 1 TB included, then €1/TB |
| Hetzner Storage Box | €3.20 (BX11) | €10.90 (BX21; 1 TB box would be full) | €10.90 (BX21) | unlimited |
| GitHub LFS (Free/Pro) | ~$12.32 | ~$64.47 | ~$194.88 | ~$80 per full 1 TB clone ($0.0875/GiB) |
| Google AI Plus / Pro | $9.99 (2 TB) | $9.99 (2 TB) | $19.99 (5 TB) | n/a (rclone, throttled) |
| OneDrive (M365 Personal) | $8.33 ($99.99/yr) | not enough (1 TB cap) | n/a | n/a |

### 7.2 Complete setups (asset store + encrypted cloud backup)

| Setup | 200 GB | 1 TB | 3 TB | Comment |
|---|---|---|---|---|
| **A. Recommended:** LFS → B2; restic → Hetzner Storage Box (SFTP) | **$1.32 + €3.20** | **$6.88 + €10.90** | **$20.78 + €10.90** | Cheapest. Two vendors, so independent failure domains. Storage Box has unlimited traffic for restore tests. |
| B. Single vendor: LFS → B2 bucket 1; restic → B2 bucket 2 (separate keys, Object Lock) | ~$2.71 | ~$13.83 | ~$41.63 | Simplest account management. Correlated vendor risk. |
| C. Zero-egress primary: LFS → R2; restic → B2 | ~$4.17 | ~$21.73 | ~$65.63 | Pick this if you'll pull the full workspace onto several machines often. |
| D. Keep on GitHub: GitHub LFS + restic → B2 | ~$13.64 | ~$71.35 | ~$215.66 | Plus ~$0.0875/GiB for every clone or download beyond 10 GiB; 2 GB file cap. **Not recommended.** |
| Optional: Azure Artifact Signing Basic | +$9.99 | +$9.99 | +$9.99 | Only if you distribute to machines with Smart App Control on (US/Canada individuals). |

Annualised for setup A: 200 GB ≈ $16 + €38/yr; 1 TB ≈ $83 + €131/yr; 3 TB ≈ $249 + €131/yr. Hetzner's flat BX21 makes the 1–5 TB range nearly constant for the backup half.

---

## 8. Recommendation

1. **GitHub:** stay on Free with private repos for `suite` (code) and `workspace` (metadata and LFS pointers). Use a self-hosted Windows runner on your PC for Qt-heavy builds; it's free and the platform fee is postponed. Hosted Windows minutes ($0.010/min) work as a fallback. Publish installers and update feeds as Release assets (each <2 GiB).
2. **Binaries:**
   - Use git-lfs + `lfs-s3` with a B2 bucket (S3 endpoint).
   - Commit `.lfsconfig` with a dummy `lfs.url` so nothing ever goes to GitHub LFS by accident.
   - The suite's workspace bootstrap sets `lfs.standalonetransferagent`, the agent path and args (no secrets), and pulls credentials from an AWS profile or Credential Manager.
   - Keep model weights out of git: list them in the manifest and fetch on demand.
   - If `lfs-s3` misbehaves, fork `lfs-dal` (Rust, OpenDAL) as a fallback. You could also eventually embed a tiny transfer agent in the suite itself; the custom-transfer protocol is simple JSON over stdin/stdout.
3. **Backups:**
   - Use restic with two repositories: one on the external drive (nightly, `--use-fs-snapshot`) and one at a Hetzner Storage Box (or a second B2 account with Object Lock).
   - Run `restic copy` or a direct backup, then `forget --prune` weekly and `check --read-data-subset=5%` monthly.
   - The suite registers the Task Scheduler jobs and surfaces `--json` status, summary and exit codes in its UI.
   - Store the repo password in Windows Credential Manager **and** offline on paper. Losing it means losing the backups.
4. **Installer:** stage with `windeployqt`, then use Velopack for a per-user install, delta auto-updates, and a feed on GitHub Releases or B2.
   - Ship the VC++ runtime DLLs app-local (see the corrected note above).
   - Start unsigned for your own machines.
   - If you're US/Canada-based and later distribute, add Artifact Signing Basic ($9.99/month).
5. **Cross-platform:** don't target it yet. Keep D3D interop and Win32 APIs behind traits and pin the Qt version.

---

## 9. Open items marked [UNVERIFIED]

- Whether included Actions minutes are drawn down at 1× for Windows under the 2026 model.
- Whether `lfs-s3` works with R2 and B2 specifically (it should, as a generic S3 endpoint).
- `lfs-standalone-file` with UNC (`\\nas\share`) paths.
- Whether restic VSS requires admin (expected yes), and Backrest GUI status.
- The Google Drive 750 GB/day upload cap for consumer accounts (it's documented for Workspace).
- Duplicacy encryption and backend list, and Borg `--json` (not re-fetched this session).
- B2 Object Lock and R2 bucket lock details (not re-fetched).
- Hetzner prices were read from the live page in a non-EU / 0% VAT context. Confirm at checkout.
- MSIX packaging details for a Win32 Qt app, and the NSIS per-user directive (not re-fetched).
- macOS developer program cost.

---

## Sources

**GitHub**
- https://docs.github.com/en/billing/concepts/product-billing/git-lfs
- https://github.com/pricing/calculator?feature=lfs
- https://docs.github.com/en/repositories/working-with-files/managing-large-files/about-large-files-on-github
- https://docs.github.com/en/repositories/working-with-files/managing-large-files/about-git-large-file-storage
- https://docs.github.com/en/repositories/creating-and-managing-repositories/repository-limits
- https://docs.github.com/en/billing/concepts/product-billing/github-actions (raw: https://raw.githubusercontent.com/github/docs/main/content/billing/concepts/product-billing/github-actions.md)
- https://docs.github.com/en/billing/reference/actions-runner-pricing
- https://github.com/resources/insights/2026-pricing-changes-for-github-actions
- https://github.blog/changelog/2025-12-16-coming-soon-simpler-pricing-and-a-better-experience-for-github-actions/
- https://github.blog/changelog/2026-01-01-reduced-pricing-for-github-hosted-runners-usage/
- https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases
- https://docs.github.com/en/actions/reference/runners/self-hosted-runners

**Git LFS and alternatives**
- https://github.com/git-lfs/git-lfs/blob/main/docs/man/git-lfs-config.adoc (LFSCONFIG allowed keys)
- https://github.com/git-lfs/git-lfs/blob/main/docs/custom-transfers.md (standalone transfer agents)
- https://github.com/nicolas-graves/lfs-s3
- https://github.com/regen100/lfs-dal
- https://github.com/jasonwhite/rudolfs
- https://github.com/datopian/giftless
- https://github.com/git-lfs/lfs-test-server
- https://github.com/twilligon/git-lfs-s3-proxy
- https://github.com/kitschpatrol/git-lfsaver
- https://github.com/sinbad/lfs-folderstore (archived)
- https://git-annex.branchable.com/install/Windows/ and https://git-annex.branchable.com/todo/windows_support/
- https://doc.dvc.org/user-guide/data-management/remote-storage/amazon-s3
- https://dvc.org/blog/dvc-joins-lakefs-your-questions-answered/ and https://lakefs.io/media-mentions/lakefs-acquires-dvc-uniting-data-version-control-pioneers/
- GitHub REST API (`api.github.com/repos/...` and `/releases/latest`) for all version and maintenance dates

**Storage pricing**
- https://www.backblaze.com/cloud-storage/pricing
- https://developers.cloudflare.com/r2/pricing/
- https://wasabi.com/pricing and https://wasabi.com/pricing/faq
- https://aws.amazon.com/s3/pricing/
- AWS Price List API: https://pricing.us-east-1.amazonaws.com/offers/v1.0/aws/AmazonS3/current/us-east-1/index.json (pub. 2026-09-28) and .../AWSDataTransfer/current/us-east-1/index.json (pub. 2026-09-16)
- https://www.idrive.com/s3-storage-e2/pricing
- https://www.hetzner.com/storage/object-storage/ and https://docs.hetzner.com/storage/object-storage/overview/
- https://www.hetzner.com/storage/storage-box/
- https://docs.hetzner.com/general/infrastructure-and-availability/price-adjustment/
- https://one.google.com/about/plans
- https://support.google.com/drive/answer/37603 and https://support.google.com/a/answer/172541
- https://www.microsoft.com/en-us/microsoft-365/buy/compare-all-microsoft-365-products
- https://www.microsoft.com/en-us/microsoft-365/onedrive/onedrive-plans-and-pricing
- https://support.microsoft.com/en-us/office/restrictions-and-limitations-in-onedrive-and-sharepoint-64883a5d-228e-48f5-b3d2-eb39e07630fa

**Backup tools**
- https://restic.readthedocs.io/en/stable/040_backup.html
- https://restic.readthedocs.io/en/stable/075_scripting.html
- https://github.com/restic/restic/blob/master/doc/030_preparing_a_new_repo.rst
- https://github.com/restic/restic/blob/master/doc/050_restore.rst
- https://kopia.io/docs/features/
- https://kopia.io/docs/reference/command-line/common/policy-set/
- https://kopia.io/docs/reference/command-line/common/snapshot-create/
- https://kopia.io/docs/mounting/
- https://github.com/kopia/kopia/releases/tag/v0.16.1 and https://github.com/kopia/kopia/issues/3798
- https://borgbackup.readthedocs.io/en/stable/installation.html
- https://github.com/borgbackup/borg/blob/master/docs/installation.rst
- https://github.com/borgbackup/borgstore
- https://duplicacy.com/buy.html and https://forum.duplicacy.com/t/backup-command-details/1077

**Installers, updates and signing**
- https://docs.firegiant.com/wix/schema/wxs/package/ and https://docs.firegiant.com/wix/osmf/
- https://jrsoftware.org/isorder.php and https://jrsoftware.org/ishelp/topic_setup_privilegesrequired.htm
- https://nsis.sourceforge.io/Download
- https://axodotdev.github.io/cargo-dist/book/installers/msi.html
- https://docs.crabnebula.dev/packager/
- https://docs.velopack.io/ , https://docs.velopack.io/packaging/installer , https://docs.velopack.io/packaging/deltas , https://docs.velopack.io/packaging/signing , https://docs.velopack.io/getting-started/rust
- https://learn.microsoft.com/en-us/windows/msix/app-installer/auto-update-and-repair--overview
- https://learn.microsoft.com/en-us/windows/msix/package/signing-package-overview
- https://winsparkle.org/ and https://github.com/vslavik/winsparkle
- https://learn.microsoft.com/en-us/azure/artifact-signing/overview
- https://learn.microsoft.com/en-us/azure/artifact-signing/quickstart
- https://learn.microsoft.com/en-us/azure/artifact-signing/how-to-change-sku
- https://azure.microsoft.com/en-us/pricing/details/artifact-signing/
- https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options
- https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/smartscreen-reputation
- https://support.microsoft.com/en-us/windows/smart-app-control-frequently-asked-questions-285ea03d-fa88-4d56-882e-6698afdb7003
- https://doc.qt.io/qt-6/windows-deployment.html

**Cross-platform**
- https://doc.qt.io/qt-6/qrhi.html
