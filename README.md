# gitpulse

Read a repository's rhythm from its own history — commit streak, weekday and hour distribution,
churn, heaviest commits. One binary, no dependencies, read-only.

```
$ gitpulse .
commits         1645
distinct days   246
range           2024-10-23 .. 2026-06-26
churn           +716811 / -601408  (1318219 changed lines)
streak          longest 25 days, current 2 days
files touched   7826

by weekday
  Mon    226  █████████████████████████
  Tue    232  ██████████████████████████
  Wed    268  ██████████████████████████████
  Thu    256  █████████████████████████████
  Fri    188  █████████████████████
  Sat    230  ██████████████████████████
  Sun    245  ███████████████████████████

by hour (local time of each commit)
  09:00     71  ████████████████
  14:00    140  ████████████████████████████████
  ...

busiest days (top 5)
  2024-12-10    38 commits
  ...

heaviest commits (top 5)
  6bff0009   527945 lines   475 files
  cec43c74   467422 lines    25 files
```

## Install

```bash
cargo install --git https://github.com/pscly1/gitpulse
```

Or build from a checkout:

```bash
git clone https://github.com/pscly1/gitpulse && cd gitpulse
cargo build --release      # target/release/gitpulse
```

## Usage

```
gitpulse [options] [path]

  --since <date>     only commits after this date (passed to git log)
  --author <name>    only commits by this author
  --top <n>          show n busiest days / heaviest commits (default 5)
  --json             machine-readable output
  --no-graph         skip the hour histogram
  -h, --help         this message
  -V, --version      print version

Exit codes: 0 ok, 1 git failure, 2 usage error
```

Point it at any path inside a repository:

```bash
gitpulse                       # current directory
gitpulse ~/code/myproject      # another repo
gitpulse . --since 2026-01-01  # this year only
gitpulse . --author alice      # one person's rhythm
gitpulse . --json | jq .longest_streak
```

## What it answers

- **Am I actually consistent?** `longest streak` and `current streak` over distinct commit days.
- **When do I do my best work?** weekday and hour histograms — useful for noticing that all your
  real work happens after 22:00.
- **How big is this repo, really?** churn totals and files touched, without `git log` paging.
- **Which commits are dangerous?** the heaviest commits are the ones worth reviewing twice.

## Design notes

- **Zero dependencies.** The whole tool is `std` plus one `git log` call. `cargo install` takes
  seconds and the binary is ~400 KB.
- **Read-only.** It never writes to the repository, never touches the index, never fetches.
- **Locale-independent.** Weekday numbers come from git's `%u` (ISO 1–7), not from `%a`, so the
  output is identical under any `LANG`.
- **Streaming parse.** `git log --numstat` output is parsed line by line: a record header carries
  hash, weekday, date, and time; the following lines are `added deleted path`.
- **Binary files are skipped** in churn (`-\t-` lines) instead of counting as zero-change edits.
- **Streaks use real calendar arithmetic** (days-from-civil), so month and leap-year boundaries do
  not break a run.

## Tests

```bash
cargo test
```

Ten unit tests cover the record parser (including the separator edge case and binary files), the
weekday mapping, streak maths across Feb 29, heaviest-commit ranking, and JSON shape.

## License

MIT
