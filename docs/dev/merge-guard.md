# Merge guard: owner's guide

This guide is for the repository owner. It explains what the merge guard does, how to set it up once, how to use it day to day, and what to do when it refuses something or when CI opens a revert PR. It is written for someone who knows GitHub's settings pages but doesn't want to read the code.

- The public repository is `xenofei/Worldwright-Suite`.
- The earlier private repository is renamed to an archive and stays private. Nothing in this guide touches it after the rename.
- The design is in the spec: rules QA-019, QA-020, QA-033 to QA-036, QA-079, QA-086 and QA-087 in `docs/superpowers/specs/2026-10-12-worldwright-suite-design.md`. The settings the tools read are in `dev/merge-guard.toml`, and the protected path patterns are in `dev/protected-paths.txt`.

Commands below are for PowerShell or Windows Terminal. `ww` means the command line built from this repository, run as `cargo run -q -p ww-cli -- dev <subcommand>` from the checkout where it is built. A `ww` built in one checkout refuses to work in another, on purpose.

Contents:

1. [What the guard does and why](#1-what-the-guard-does-and-why)
2. [One-time setup](#2-one-time-setup)
3. [Everyday use](#3-everyday-use)
4. [When CI opens a revert PR and an issue](#4-when-ci-opens-a-revert-pr-and-an-issue)
5. [Rotating the agent token](#5-rotating-the-agent-token)
6. [Maintaining the privacy denylist](#6-maintaining-the-privacy-denylist)
7. [First run](#7-first-run)

---

## 1. What the guard does and why

Agents write most of the code here. The guard makes sure two things stay true however an agent behaves:

- A change to a **protected file** reaches `main` only after the reviewer App has approved that exact commit, and the App can approve only when you type its passphrase at a terminal.
- Agents merge **only** with `ww dev merge <PR>`, which checks everything first.

Protected files are the ones that decide whether the tests and the guard itself can be trusted: spec tests, golden files, recorded fixtures, committed property-test seeds, `test-params.toml`, the hook scripts, the sources of `ww dev merge` and the guard, the settings in `dev/`, every workflow under `.github/workflows/`, `.claude/**`, `.cargo/**`, and a few build files (QA-019). The authoritative list is `dev/protected-paths.txt`. Only **adding** a file under the test homes (`crates/ww-spec-tests/**` and `**/proptest-regressions/**`) is free. Modifying or deleting any protected file, or adding one anywhere else, needs approval (QA-020).

No single mechanism is trusted on its own. A local check can be skipped, so each one has a second layer behind it.

### The layers

| Layer | What it does |
| --- | --- |
| **Git hooks** (`tools/git-hooks`) | `pre-push` refuses any push that changes `main` and runs the privacy scan on every pushed branch and tag. `pre-merge-commit` refuses a local merge into `main`. Installed by `ww dev install-hooks`. |
| **Credential helper** | `ww dev install-hooks` also makes git in that clone use only the agent token (`ww dev git-credential`), not any personal login. |
| **`ww dev merge <PR>`** | Merges through GitHub's merge API, pinned to the exact head commit it checked. Refuses unless CI is green, protected changes are approved, and the hooks and credential helper are installed. |
| **Ruleset on `main`** (GitHub) | Requires a pull request and the four checks below, blocks force-push and deletion, no bypass. This is the server-side backstop. |
| **CI checks** | See the next table. |
| **Post-merge check** (`post-merge.yml`) | Runs after every push to `main`. If a commit got in without a merged PR, or a protected change got in without the App's approval, or the tests fail, it opens a **revert PR** and an **issue** assigned to the owner (section 4). |
| **Agent token** (QA-087) | A fine-grained token limited to this repository, with no Administration and no Workflows permission and an expiry. It is the only credential on the machine agents run on. |
| **Read-only default token** (QA-079) | Workflows get a read-only token. Only the `post-merge-check` job can write, so it can open the revert PR and issue. |

### The four required checks

| Check | Workflow | What it covers |
| --- | --- | --- |
| `every-push` | `ci-push.yml` | Runs on every push to a branch other than `main`: rustfmt, clippy, cargo-deny, the workspace tests, the spec tests and the spec traceability report. |
| `pr-ready` | `ci-pr-ready.yml` | Runs on every pull request: everything `every-push` runs, plus cargo-audit and a release build. On a **draft** PR it fails on purpose (a skipped required check would count as passing). Marking the PR ready for review runs it again. |
| `protected-test-check` | `ci-guard.yml` | Judges the PR's head commit with `ww` built from `main` (the PR's own code is never run). Fails when a protected file is modified or deleted without the reviewer App's approval of that exact commit. Runs when a PR is opened, pushed to or reopened (not on reviews, not on draft-to-ready), and never cancels a run for the same commit. Every run is its own check suite, so a commit can have more than one (after a close and reopen) and the merge needs all of them green; `ww dev approve` re-runs the red ones after posting the approval. |
| `privacy-check` | `ci-push.yml` | Runs `ww dev privacy-scan` on every commit of the pushed branch that `main` lacks. It fails if the `WW_PRIVACY_DENYLIST` secret is missing or empty. |

A second privacy job, `privacy-check-pr`, runs on pull requests and shows the same verdict there. It is not a required check, because a required check name must come from exactly one job. `nightly.yml` (release build and coverage) is not required either.

### The privacy scan (QA-086)

The repository is public, so every file, path and commit is public. The scan looks for:

- Windows user-profile paths (write `%USERPROFILE%` instead).
- E-mail addresses other than GitHub no-reply addresses, the `noreply@anthropic.com` co-author trailer and reserved test domains.
- Commit author and committer identities that aren't GitHub no-reply addresses.
- Anything in the **private denylist**: your real e-mail addresses, name and username forms, one per line. A denylist finding names only the entry's number and the place, never the entry.

The denylist is kept in two places that you keep in sync (section 6): the file `%USERPROFILE%\.worldwright\privacy-denylist.txt` on your machine, and the Actions secret `WW_PRIVACY_DENYLIST` for CI. It is never committed.

### What the guard can't do

- It can't stop someone who has a full-access login to the repository. That is why that login is removed from the PC agents use (step 2.7).
- It trusts the passphrase prompt of `ww dev approve`. Build the `ww` you approve with from a clean checkout that agents can't write to, so a modified binary can't capture the passphrase.

---

## 2. One-time setup

Do these in order. Steps marked **browser** are done on github.com. The tools never create or change repository settings for you.

### 2.1 Owner preparation (browser, then PowerShell)

1. **Profile check.** Open your GitHub profile settings (Settings → Public profile) and look at everything public: name, bio, location, company, links and the public e-mail. Remove anything you don't want visible next to the repository.
2. **E-mail privacy.** In Settings → Emails (or the equivalent current location), turn on both:
   - "Keep my email addresses private"
   - "Block command line pushes that expose my email"

   That page also shows your no-reply address, in the form `<id>+<login>@users.noreply.github.com`.

   GitHub creates the merge commit itself when `ww dev merge` merges a PR, and it records that commit with the e-mail your account's web settings give it. No hook or pre-push scan on your PC ever sees that commit. So "Keep my email addresses private" must be on **before the first `ww dev merge`**. To verify it, merge a throwaway PR in a private repository on github.com, fetch it, and check the merge commit's author e-mail (`git log -1 --format=%ae` on that repository's `main`): it must be your no-reply address. If a commit with any other address still reaches `main`, the post-merge check flags it afterwards: its identity check (`ww dev privacy-scan --commits`) goes red and opens an issue assigned to you. By then the commit is public and a revert doesn't remove it from history, so fix the setting and consider rewriting `main` or recreating the repository.
3. **Global no-reply address.** Tell git to use it everywhere:

   ```powershell
   git config --global user.email "<id>+<login>@users.noreply.github.com"
   ```

   Every repository-local `user.email` below uses the same address. Set it **before the first commit** in every clone. The privacy scan refuses commits made with any other address.
4. **Write the denylist first.** Create `%USERPROFILE%\.worldwright\privacy-denylist.txt` before anything is pushed:

   ```powershell
   New-Item -ItemType Directory -Force "$env:USERPROFILE\.worldwright" | Out-Null
   notepad "$env:USERPROFILE\.worldwright\privacy-denylist.txt"
   ```

   One entry per line: your real e-mail addresses, your real name in the forms it might appear, and your username forms. Matching is case-insensitive and finds the entry anywhere inside a line or path. Blank lines and lines starting with `#` are ignored. Entries shorter than 5 characters work but produce a warning, because they can match random data. The `pre-push` hook refuses every push until this file exists, and the privacy scan refuses to run without it.
5. **Rename the earlier private repository** (Settings → General → Repository name) to an archive name, so the public repository can use the name `Worldwright-Suite`. Keep it private. Don't use clones of the archive for the new repository: `ww` checks that `origin` points to `github.com/xenofei/Worldwright-Suite` and refuses otherwise.

### 2.2 Building the initial commit (PowerShell)

The public repository starts from one clean commit made from the tree of `plan-1-foundation`, so none of the earlier history becomes public.

In the repository that holds `plan-1-foundation`:

```powershell
$source = (Get-Location).Path
$export = "<a new, empty folder outside this repository>"
New-Item -ItemType Directory $export | Out-Null
git archive --format=tar --output="$export.tar" plan-1-foundation
tar -xf "$export.tar" -C $export
git rev-parse 'plan-1-foundation^{tree}'      # note this id
```

Then in the export:

```powershell
Set-Location $export
git init -b main
git config user.email "<id>+<login>@users.noreply.github.com"
git add -A
git update-index --chmod=+x tools/git-hooks/pre-push tools/git-hooks/pre-merge-commit
$tree = git write-tree
$tree                                          # must equal the id printed above
git ls-files -s tools/git-hooks                # both hooks must show mode 100755
```

Notes:

- Name **both** hook files. PowerShell doesn't expand `*`, so `tools/git-hooks/*` would fail.
- A Windows `git add` doesn't keep the executable bit. Without `--chmod=+x` the hooks are not executable, which would silently weaken the guard.
- If `$tree` differs from the source tree id, **stop**. Something changed in the export (line endings, a missing file, a stray file). Find out what before going on.

Next:

1. If you chose a repository name other than `Worldwright-Suite`, change `repo` in `dev/merge-guard.toml` and `git add` it. With the name `Worldwright-Suite`, nothing changes.
2. Compare the file lists, modes and object ids with the source. Nothing should be printed (only the `dev/merge-guard.toml` line if you edited it):

   ```powershell
   $a = git -C $source ls-tree -r plan-1-foundation
   $b = git ls-tree -r (git write-tree)
   Compare-Object $a $b
   ```
3. Make the one commit:

   ```powershell
   git status --short                            # only staged files, nothing else
   git commit -m "Initial public commit"
   ```
4. **Build `ww` in the export** and run the privacy scan with it. Don't use a `ww` built elsewhere: a `ww` from another checkout refuses to work here, and the privacy gate has to be this checkout's own build.

   ```powershell
   cargo run -q -p ww-cli -- dev privacy-scan --tree HEAD
   cargo run -q -p ww-cli -- dev privacy-scan --commits HEAD
   ```

   Both must finish with exit code 0 (`$LASTEXITCODE`). `--tree HEAD` scans every tracked file's content and path. `--commits HEAD` checks the identities of every commit. Exit 1 means findings (fix them and redo this section from the export), exit 2 means the scan couldn't run (usually a missing denylist).

### 2.3 Creating and pushing the repository (browser, then PowerShell)

1. **Browser:** create the repository `Worldwright-Suite` under your account (New repository). Make it **Public**, and add no README, licence or `.gitignore`: it must start empty. (If you prefer the command line, `gh repo create xenofei/Worldwright-Suite --public` does the same with your own login.)
2. Push from the export:

   ```powershell
   git remote add origin https://github.com/xenofei/Worldwright-Suite.git
   git push -u origin main
   ```

   The export has no hooks installed, so the push runs no hook. That is why section 2.2 ran the scan by hand first.
3. The push to `main` starts `post-merge.yml`. Its tests run, and its judge step logs `repository creation push; nothing to compare`. That is expected for the first push only.

### 2.4 Repository settings (browser)

The names below are GitHub's at the time of writing. If a page has moved, look for the equivalent current location. Settings that aren't offered on your plan or account are a reason to stop and ask, not to skip.

**Ruleset on `main`** (Settings → Rules → Rulesets, or the equivalent current location). Create a branch ruleset, enforcement **Active**, targeting the default branch (`main`):

- **Require a pull request before merging**, with **0** required approvals. The reviewer App's approval is checked by the guard, not by the ruleset.
- Allowed merge methods include **merge commit**. `ww dev merge` uses the merge method.
- **Do not** turn on "Require linear history".
- **Do not** turn on "Require branches to be up to date before merging" (strict status checks).
- **Require status checks to pass**, with exactly these four, each from the **GitHub Actions** source: `every-push`, `pr-ready`, `protected-test-check`, `privacy-check`.
- **Block force pushes** and **restrict deletions**.
- **No bypass list.** Not even you.

If the check picker offers only checks that have already run in the repository, push a throwaway branch and open a throwaway PR first so the four names exist, then add them to the ruleset. Add the ruleset as soon as the names are available.

**Other settings:**

1. **Pull requests restricted to collaborators** (Settings → General, pull request creation, or the equivalent current location).
2. **Fork-workflow approval:** Settings → Actions → General → fork pull request workflows → **Require approval for all outside contributors**.
3. **Secret scanning push protection:** on (Settings → Advanced Security / Code security, or the equivalent current location).
4. **Default workflow token read-only:** Settings → Actions → General → Workflow permissions → **Read repository contents and packages permissions**.
5. **Allow GitHub Actions to create pull requests: on.** It is the checkbox just below the previous setting. GitHub labels it "Allow GitHub Actions to create and approve pull requests". It is needed so the post-merge check can open a revert PR. A workflow's approval never counts toward the guard, because only the reviewer App's approval does. With it off, the post-merge check still opens the issue, and says it couldn't open the PR. You can set 4 and 5 together with your own login:

   ```powershell
   gh api -X PUT repos/xenofei/Worldwright-Suite/actions/permissions/workflow -f default_workflow_permissions=read -F can_approve_pull_request_reviews=true
   ```

   (The `can_approve_pull_request_reviews` field is the same setting as the checkbox. Setting it to `false` would stop revert PRs.)
6. **The denylist secret.** Settings → Secrets and variables → Actions → **New repository secret**, named `WW_PRIVACY_DENYLIST`, with the **whole contents** of the denylist file as its value. It must be a repository secret, not an environment secret. From the command line with your own login:

   ```powershell
   Get-Content -Raw "$env:USERPROFILE\.worldwright\privacy-denylist.txt" | gh secret set WW_PRIVACY_DENYLIST --repo xenofei/Worldwright-Suite
   ```

   Secrets can't be read back, so keep the file as the master copy. A missing or empty secret makes `privacy-check` and `privacy-check-pr` fail. Pull requests from forks never receive secrets, so they fail those checks by design.
7. **Issues, collaborators only.** Open issues to collaborators only:
   - If Settings → General offers a "collaborators only" choice for issues, use it.
   - If it doesn't, use an interaction limit: Settings → Moderation options → Interaction limits (or the equivalent current location) → limit to repository collaborators, for **6 months**, the longest duration offered. The limit lapses by itself. **Renew it before it ends**, and put the date in your calendar: after that, anyone can open issues again.

   CI-opened revert issues are assigned to you. If you ever expect a revert issue and don't find one, look at the post-merge run itself, which shows red.

### 2.5 Creating the reviewer GitHub App (browser, then PowerShell)

The reviewer App is the second identity that approves protected changes. Agents can't act as it.

1. **Browser:** Settings → Developer settings → GitHub Apps → **New GitHub App**.
   - **Name:** `worldwright-reviewer`. (App names are unique across GitHub. If it's taken, pick another and set `reviewer_app_slug` in `dev/merge-guard.toml` to match, which is a protected change.)
   - **Homepage URL:** any URL, for example the repository's.
   - **Webhook:** turn **Active** off.
   - **Repository permissions:** **Pull requests: Read and write**, **Contents: Read-only**, **Actions: Read and write**. (Metadata read-only is added automatically.) Nothing else. Actions write is there so that an approval re-runs the guard check: `protected-test-check` runs only on pull request events, so its run for a commit happened before your approval and failed. Right after posting the approval, `ww dev approve` asks GitHub to re-run that run; the re-run reads the reviews again and passes. The App uses this permission for nothing else.
   - **Where can this GitHub App be installed?** **Only on this account**.
   - Create the App.
2. On the App's settings page, note the **App id** (a number). Under Private keys, **Generate a private key**. A `.pem` file downloads.
3. **Install the App** (the App's Install App page): choose your account, **Only select repositories**, and pick `Worldwright-Suite`. After installing, the address of the installation's page ends in the **installation id** (a number, `.../settings/installations/<number>`). Note it.
4. **Run `ww dev reviewer-setup`** from **PowerShell or Windows Terminal**, not from mintty (Git Bash's default terminal), where the hidden passphrase input can hang. Run it in a checkout where `ww` was built (the export, or the fresh clone from 2.6):

   ```powershell
   cargo run -q -p ww-cli -- dev reviewer-setup
   ```

   It asks for the App id, the installation id, the path to the `.pem` and a passphrase (at least 12 characters, typed twice). It writes the key, encrypted with that passphrase, to `%USERPROFILE%\.worldwright\reviewer\app-key.age`. It only runs at a real terminal. Keep the passphrase somewhere safe: if it is lost, generate a new private key on the App's page, delete the old one there, and run `reviewer-setup` again.
5. **Delete the `.pem` yourself.** `ww` never deletes or changes it. Delete it permanently (Shift+Delete, or empty the Recycle Bin) and check the Downloads folder for copies.

**If the App was created before it needed Actions write** (it then has only Pull requests and Contents), add the permission once. Nothing on your PC changes: the key, the App id and the installation id stay the same.

1. **Browser, the App's settings:** Settings → Developer settings → GitHub Apps → `worldwright-reviewer` → **Edit** (or the equivalent current location) → **Permissions & events** → **Repository permissions** → **Actions** → **Read and write** → **Save changes** at the bottom of the page.
2. **Browser, the installation:** the installation must accept the new permission before it applies. Open the installation's page (Settings → Applications → Installed GitHub Apps → `worldwright-reviewer` → **Configure**; its address ends in the installation id). A **Review request** banner at the top says the App asks for new permissions. Click it, check that the only addition is Actions read and write, and **Accept new permissions**.

Until the installation accepts, `ww dev approve` still posts approvals, then prints the warning described in section 3 ("the reviewer app's installation hasn't granted Actions: Read and write") and you re-run the check by hand.

### 2.6 Creating the agent token (browser, then PowerShell)

1. **Browser:** Settings → Developer settings → Personal access tokens → **Fine-grained tokens** → Generate new token.
   - **Resource owner:** your account.
   - **Repository access:** only select repositories → `Worldwright-Suite` only.
   - **Repository permissions:**

     | Permission | Access |
     | --- | --- |
     | Contents | Read and write |
     | Pull requests | Read and write |
     | Issues | Read and write |
     | Actions | Read |
     | Checks | Read |
     | Metadata | Read |

     **No** Administration. **No** Workflows. Nothing else.
   - **Expiration:** set one. Note the date: section 5 needs it.
2. Make a **fresh clone** of the public repository. It becomes the agents' working folder:

   ```powershell
   git clone https://github.com/xenofei/Worldwright-Suite.git <work folder>
   Set-Location <work folder>
   git config user.email "<id>+<login>@users.noreply.github.com"
   ```

3. In that clone, from a real terminal:

   ```powershell
   cargo run -q -p ww-cli -- dev agent-token set
   cargo run -q -p ww-cli -- dev install-hooks
   ```

   `agent-token set` reads the token without echo, then asks for the expiry date GitHub showed (`YYYY-MM-DD`, or `none`). It stores the token in Windows Credential Manager as `Worldwright-dev/github/agent-token`. `install-hooks` sets `core.hooksPath` to `tools/git-hooks` and makes git in this clone use only the agent token. Check with `ww dev agent-token status`, which shows the first four characters and the expiry you entered.

### 2.7 Removing the full-access login from the agents' PC

Do this last, and only on the PC agents run on, after step 2.6 works. Agents must have no credential stronger than the agent token.

1. Log `gh` out:

   ```powershell
   gh auth logout --hostname github.com
   gh auth status          # should say you are not logged in
   ```

2. Remove the Git Credential Manager entry for github.com: open Windows Credential Manager (search for it in the Start menu), choose **Windows Credentials**, find `git:https://github.com` under Generic Credentials, expand it and choose **Remove**. Leave the `Worldwright-dev/github/agent-token` entry alone: that is the agent token.
3. From then on, do owner-only work (settings, App and token pages) in the browser, and keep any personal full-access login off this PC. Agents can read and manage PRs with `ww dev gh -- <gh arguments>` (see `ww dev gh --help` for what it allows).

---

## 3. Everyday use

### What agents do

1. Work in the fresh clone, on a branch (never `main`).
2. Push the branch (the hooks run the privacy scan on every push) and open a pull request, as a draft or ready.
3. Mark it ready, wait for the four checks, then run:

   ```powershell
   cargo run -q -p ww-cli -- dev merge <PR>
   ```

   It merges only when CI is green for the PR's current head commit, any protected changes are approved, and the merge can be pinned to that commit.

Agents never run `ww dev approve`, never use `gh pr merge`, never push to `main` and never use `--no-verify`. The project's Claude Code settings deny those, and the hooks, the ruleset and the post-merge check back that up.

### What the owner does

You only act when an agent asks for approval of a protected change:

1. Read what changed. `ww dev approve` lists the protected files, but not their contents, so look at the diff of those files in the PR on GitHub first.
2. In **PowerShell or Windows Terminal** (not mintty), run it in a clean checkout of `main` that agents can't write to. A separate clone used only for this works well. Update it first:

   ```powershell
   git status --short            # must print nothing
   git pull --ff-only
   cargo run -q -p ww-cli -- dev approve <PR>
   ```

   It prints the commit and the protected changes, then asks `(y/N)`. If you answer `y`, it asks for the reviewer key's passphrase, then posts one approving review from the App, pinned to that exact commit. It needs a GitHub token to read the PR, so run it where the agent token is stored (`ww dev agent-token status`), and the key file from 2.5 has to be on that PC.
3. Right after posting, `ww dev approve` asks GitHub to re-run every red `protected-test-check` run of this PR for that commit and says so ("Asked GitHub to re-run protected-test-check (workflow runs ...)"). It only touches this PR's runs from this repository, never a fork's, and never runs waiting for a maintainer's approval (`action_required`). If a run is still going, it first waits for it to finish, for up to a minute. The re-runs see your approval and turn green within a few minutes. If the check had already passed, it says so and re-runs nothing. The agent then runs `ww dev merge <PR>`.

   If it prints **"Approval posted, but the guard check is still running; it will most likely read your approval when it finishes. If it ends red, re-run it: ..."**, a run was still going after the minute. GitHub re-runs only finished runs, so `ww` leaves it alone. Usually it reads your approval and ends green. If it ends red, re-run it as below.

   If it prints **"Approval posted, but the guard check couldn't be re-run automatically (...). Re-run it in the browser: ..."**, the approval is posted and only the re-run is missing. Open the address it gives, which is the check's run or the PR's checks page, signed in as yourself, and use **Re-run jobs** → **Re-run all jobs** on each red `protected-test-check` run (the "CI: protected-test check" workflow). Don't push a new commit just to trigger it: a new commit needs a new approval. The reason in brackets says what to fix for next time:

   | Reason (shortened) | What to do |
   | --- | --- |
   | "GitHub refused the installation-token exchange with HTTP 403/422: ...; the reviewer app's installation has probably not granted Actions: Read and write" | GitHub's own message comes first. Usually it means the permission is missing: add it (section 2.5, the part about an App created before it needed Actions write), and accept it on the installation. |
   | "GitHub refused to re-run guard run ...: ... HTTP 403 ..." | Usually the same missing permission, or the request was accepted on the App but not yet on the installation. Check both pages from section 2.5. |
   | A refused re-run of a run that is about a month old | GitHub re-runs only runs from the last 30 days or so, and the browser can't re-run it either. Push a new commit to the PR (an empty one is enough: `git commit --allow-empty -m "run checks"`); it starts a fresh run, and that commit needs a new `ww dev approve <PR>`. |
   | "no `pull_request` run of ci-guard.yml was found for PR #..." | The run hasn't been created yet or GitHub didn't list it. Check the PR's checks page: if the check is there and red, re-run it; if it's still running, wait. |
   | "guard run ... ended as action_required and isn't re-run" | The run waits for a maintainer's approval to start at all. Approve it on the run's page, or close it and push from a branch of this repository. |

   If it says **"the PR head moved after it"**, a commit was pushed while you approved. The approval doesn't count for the new commit: run `ww dev approve <PR>` again.

   The App's installation token lives only while `ww dev approve` runs: it is revoked when the command ends, also after an error. If you stop the command with Ctrl+C (for example during the minute's wait), it can't revoke the token, which then stays valid until it expires on its own, within an hour.

An approval covers one commit. If anything is pushed to the PR after you approve, the old approval no longer counts and you will be asked again.

### What a refusal means

`ww dev merge` says in plain words which condition failed, and merges nothing.

| What it says (shortened) | What it means | What to do |
| --- | --- | --- |
| "Protected files changed and the reviewer app hasn't approved this exact commit: ..." | The PR changes protected files and the App hasn't approved this commit, or approved an older one. | The owner runs `ww dev approve <PR>` when asked (above). Agents don't retry. |
| "The PR's head moved after the checks passed, so GitHub refused the pinned merge." | A commit was pushed between the check and the merge. Nothing was merged. | Run `ww dev merge <PR>` again. If the PR has protected changes, it needs a fresh approval of the new head. |
| "CI check ... hasn't reported for ... yet" / "is still running" / "finished as failure, not success" | A required check hasn't run, hasn't finished or failed on that exact commit. | Wait, or fix the failure and push. `every-push` and `privacy-check` start on a push, while `pr-ready` and `protected-test-check` start on PR events. A draft PR fails `pr-ready` on purpose: mark it ready for review. |
| "The git hooks aren't installed ..." or "The agent credential helper isn't installed ..." | This clone isn't set up, or someone changed `core.hooksPath` or the credential helper. | Run `ww dev install-hooks` in the clone. Nothing was contacted. |
| "Privacy scan (QA-086): this push is refused." (from the `pre-push` hook), or `privacy-check` red | The scan found a user path, a stray e-mail address, a commit identity that isn't a no-reply address, or a denylist entry. Denylist findings give only the entry's number and the place. | Remove or replace each finding and **rewrite the commits that hold it** (amend or rebase): the scan covers every commit of the branch, so a later clean commit doesn't help. Set the clone's `user.email` to the no-reply address. Don't use `--no-verify`. |
| "privacy denylist not found; see docs/dev/merge-guard.md" or "privacy denylist has no entries ..." | The file `%USERPROFILE%\.worldwright\privacy-denylist.txt` is missing or empty (or `WW_PRIVACY_DENYLIST` is set to nothing useful). | Create it (section 2.1, step 4). In CI, a missing or empty secret fails the same way: section 6. |
| "Pushes to main go through `ww dev merge <PR>`." | Someone tried to push to, or merge locally into, `main`. | Open a PR and use `ww dev merge`. |
| "The PR is a draft", "is closed", or "targets ..., not main" | The PR can't be merged as it is. | Mark it ready, reopen it, or retarget it. |
| "Couldn't finish the checks, so the merge is refused: ..." or "No GitHub access ..." | GitHub or git couldn't be reached, or the agent token is missing, expired or rejected. | Retry. If it persists, check `ww dev agent-token status` and section 5. Nothing is ever merged when a check can't be made. |
| "The merge request to GitHub failed ... It may or may not have merged" | The merge call failed halfway. | Look at the PR on GitHub before trying again. |
| "this ww was built for ...; run it from there ..." or "ww refuses to run with GIT_DIR set ..." | `ww` was built in a different checkout, or git's `GIT_DIR` or `GIT_WORK_TREE` variables are set. | Build and run `ww` in the checkout you are working in (`cargo run`), and unset those variables. |

`ww dev approve` has its own refusals:

| What it says | What to do |
| --- | --- |
| "only runs at a real terminal" | Run it from PowerShell or Windows Terminal, not from a script or an agent session. |
| "there is no reviewer key at ...; run `ww dev reviewer-setup` first" | Do section 2.5 on this PC. |
| "nothing protected to approve" | The PR changes no protected file, so it needs no approval. |
| "wrong passphrase; nothing was posted" | Try again. |
| "the PR head moved since the summary; nothing was approved" | The PR changed while you read it. Run `approve` again and read the new summary. |
| "PR #... is closed / a draft / targets ..." | Fix the PR first. |

---

## 4. When CI opens a revert PR and an issue

### What it means

Every push to `main` is checked after the merge. If a commit reached `main` without a merged PR, or a protected change reached it without the reviewer App's approval of the PR's final head commit, or the tests on the pushed commit failed, then `post-merge-check` goes red and opens:

- a branch `revert/<first 7 characters of the new main>...`;
- a **pull request** into `main` that reverts everything main took in that push;
- an **issue assigned to you**, with the verdict in plain language.

It is a warning that something got past the earlier layers, which should be rare. If the revert can't be made automatically (a conflict), only the issue is opened, and it says so. If `main` has been rewritten, or has no `ww` to judge with, it opens a plain issue and the run stays red. A **cancelled** post-merge run can stop before judging. Treat a cancelled run on `main` as something to look at by hand.

### Getting checks on the revert PR

The revert PR is opened with the workflow's own token, and GitHub starts no workflows for events caused by that token. So the revert PR has **no checks**, and `ww dev merge` won't merge it. To get them:

- **Push an empty commit** to the revert branch, as yourself, using a clone with the no-reply address set:

  ```powershell
  git fetch origin
  git switch revert/<branch name from the PR>
  git commit --allow-empty -m "run checks"
  git push
  ```

  A push starts all four required checks. The agents' clone can do this with the agent token. If GitHub refuses the push, push it with your own login from another PC.
- Closing and reopening the PR only starts the checks that run on pull request events (`pr-ready`, `protected-test-check`). `every-push` and `privacy-check` run on pushes, so closing and reopening alone leaves the PR short of two required checks. Use the empty commit.

### Merging or closing it

1. Read the issue. It names the commit and says what was flagged.
2. **Decide whether the change should stay on `main`.**
   - **Revert it:** get the checks as above. A revert of a protected change is itself a protected change, so it needs `ww dev approve <PR>` from you. Then run `ww dev merge <PR>`.
   - **Keep it:** close the revert PR with a short comment saying why, and close the issue. If a protected change went in unapproved, think about why before closing. The usual causes are an approval of an earlier commit, or a merge that wasn't made through `ww dev merge`.
3. If only an issue was opened, revert by hand: make a branch, `git revert`, open a PR, and merge it the usual way.

A flagged push is not flagged again by later pushes. Each post-merge check judges only its own push.

---

## 5. Rotating the agent token

The agent token has an expiry. `ww` doesn't enforce the date (it only records what you entered and shows it in `ww dev agent-token status`), so GitHub rejecting the token is how you'll find out. Put the date in your calendar well before it.

**Rotate before it expires:**

1. **Browser:** Settings → Developer settings → Personal access tokens → Fine-grained tokens. Generate a new token with the same settings as section 2.6, or regenerate the existing one if GitHub offers it. Set a new expiry.
2. In the agents' clone, from a real terminal:

   ```powershell
   cargo run -q -p ww-cli -- dev agent-token set
   ```

   It says a token is already stored and asks `Replace it? (y/N)`. Answer `y`, paste the new token and enter the new expiry date.
3. Check it: `cargo run -q -p ww-cli -- dev agent-token status`, and try `ww dev gh -- pr list`.
4. **Browser:** delete the old token.

**If it expires:** every GitHub call agents make fails: `ww dev merge` refuses ("No GitHub access" or "Couldn't finish the checks"), pushes through the credential helper fail to authenticate, and `ww dev gh` fails. Nothing becomes less safe, since a check that can't be made is a refusal, but work stops until you rotate. CI is not affected, because it uses its own workflow token. The reviewer App's key doesn't expire, so approvals keep working, except that `ww dev approve` also needs a stored token to read the PR.

To remove the token completely (for example if it leaked), run `ww dev agent-token clear`, delete it on GitHub, and create a new one.

---

## 6. Maintaining the privacy denylist

- **Add entries** to `%USERPROFILE%\.worldwright\privacy-denylist.txt`: one per line, any new e-mail address, name form or username form. Matching is case-insensitive and finds the entry anywhere. Lines starting with `#` and blank lines are ignored. Don't make entries shorter than 5 characters unless you accept the warning.
- **Never commit it.** It holds exactly what the repository must not contain. Keep it out of any folder that is, or will be, tracked. The scan never prints an entry: it names only the entry's number (counted among non-blank, non-comment lines) and the file and line.
- **Keep the file and the secret in sync.** CI reads the secret, not your file. After every change, update the secret with the whole file (the command is in section 2.4, step 6, or use the secret's page in Settings → Secrets and variables → Actions). A secret can't be read back, so the file is the master copy. If the two differ, local pushes and CI will disagree.
- **Careful with `WW_PRIVACY_DENYLIST` on your own PC.** When that environment variable is set, `ww` uses it and ignores the file.
- **After adding an entry,** check whether the public history already contains it:

  ```powershell
  cargo run -q -p ww-cli -- dev privacy-scan --tree HEAD
  cargo run -q -p ww-cli -- dev privacy-scan --commits HEAD
  ```

  A single revision means its whole history. Findings in `main` can't be removed with a normal PR: treat that as an incident, and decide with care whether to rewrite history.
- If `privacy-check` fails with "The WW_PRIVACY_DENYLIST secret is missing or empty", the secret was deleted, renamed or emptied. Set it again.

---

## 7. First run

The dry runs against the public repository prove the guard works before real work depends on it. Fill the table in through a PR merged with `ww dev merge`. The record lives on a follow-up branch, since `main` holds the guard.

The dry runs, each with the expected outcome:

1. **A protected change is refused, then approved, then merged.** On a branch `guard-dry-run`, change only a comment in `test-params.toml` and open a PR. `protected-test-check` fails. `ww dev merge <PR>` refuses with the "Protected files changed and the reviewer app hasn't approved this exact commit" message. The owner runs `ww dev approve <PR>`, which re-runs the check, the check passes, and `ww dev merge <PR>` merges. Repeat it once with a PR opened as a **draft**: open it as a draft, wait for `protected-test-check` to fail, mark it ready for review, then approve and merge as before. Marking it ready must not add a second `protected-test-check` run for the commit (only `pr-ready` runs again), and the merge must not be refused for a cancelled check.
2. **A stale approval is refused.** On a second throwaway PR, approve it, then push one more commit. The old approval no longer counts and the merge is refused. Close or delete the throwaway branches.
3. **A privacy dry run.** On a throwaway branch, add a file that contains a Windows profile path with a name that isn't a placeholder. The `pre-push` hook refuses the push. To see `privacy-check` go red as well, push the same branch once from a clone that has no hooks installed (your own clone, not the agents'). Never merge it, and delete the branch afterwards: the commit is public while it exists.
4. **An agent-token check.** With the agent token, push a change under `.github/workflows/` to a throwaway branch. GitHub rejects it, since the token has no Workflows permission.
5. **The post-merge check.** The first merged PR is followed by a green `post-merge-check` on `main`.

| Date | Check | URL | Outcome |
| --- | --- | --- | --- |
| | | | |
| | | | |
| | | | |
