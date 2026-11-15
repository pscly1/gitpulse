//! gitpulse — read a repository's rhythm from its own history.
//!
//! Shells out to `git log` and analyses the stream in-process. No crates, no
//! network, no writes: it only reads.

use std::collections::HashMap;
use std::env;
use std::process::{Command, Stdio};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone)]
struct Commit {
    hash: String,
    timestamp: String,
    weekday: usize,
    hour: usize,
    files: usize,
    insertions: usize,
    deletions: usize,
}

impl Commit {
    /// Changed lines in this commit. Used by the report and by tests.
    fn churn(&self) -> usize {
        self.insertions + self.deletions
    }
}

#[derive(Debug, Default)]
struct Report {
    total: usize,
    heaviest: Vec<(String, usize, usize)>, // (short hash, files touched, changed lines)
    files_touched: usize,
    months: Vec<(String, usize)>, // "YYYY-MM" -> commits, sorted
    omissions: usize,             // commits whose weekday could not be read
    insertions: usize,
    deletions: usize,
    by_weekday: [usize; 7],
    by_hour: [usize; 24],
    top_days: Vec<(String, usize)>,
    longest_streak: usize,
    current_streak: usize,
    first_date: String,
    last_date: String,
    distinct_days: usize,
}

fn usage() -> String {
    format!(
        "gitpulse {VERSION} — read a repo's rhythm from its own history

Usage: gitpulse [options] [path]

Options:
  --since <date>     only commits after this date (passed to git log)
  --author <name>    only commits by this author
  --top <n>          show n busiest days (default 5)
  --json             machine-readable output
  --no-graph         skip the hour histogram
  -h, --help         this message
  -V, --version      print version

Exit codes: 0 ok, 1 git failure, 2 usage error"
    )
}

/// git's `%u` is ISO weekday: 1 = Monday .. 7 = Sunday. We store 0..6 from Monday.
fn iso_weekday_index(tok: &str) -> usize {
    match tok.trim().parse::<usize>() {
        Ok(n) if (1..=7).contains(&n) => n - 1,
        _ => 0,
    }
}

/// Parse one `git log` record. Records are separated by \x1e, fields by \x1f.
fn parse_record(record: &str) -> Option<Commit> {
    // Callers may hand us the raw record including its leading separator.
    let record = record.trim_start_matches('\u{1e}');
    // Shape: "<hash>\x1f<weekday>|<date>|<time>\n<numstat lines...>"
    let mut parts = record.splitn(2, '\u{1f}');
    let hash = parts.next()?.trim().to_string();
    if hash.is_empty() {
        return None;
    }
    let body = parts.next().unwrap_or("");
    let mut lines = body.lines();

    // Header line: ISO weekday | YYYY-MM-DD | HH:MM:SS
    let header = lines.next().unwrap_or("");
    let mut cols = header.split('|');
    let weekday = iso_weekday_index(cols.next().unwrap_or("1"));
    let date = cols.next().unwrap_or("").trim().to_string();
    let time = cols.next().unwrap_or("00:00:00").trim();
    let hour: usize = time
        .split(':')
        .next()
        .and_then(|h| h.parse().ok())
        .unwrap_or(0);

    let mut files = 0usize;
    let mut insertions = 0usize;
    let mut deletions = 0usize;
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut c = line.split_whitespace();
        let a = c.next().unwrap_or("-");
        let d = c.next().unwrap_or("-");
        if a == "-" || d == "-" {
            continue; // binary file: numstat prints "-\t-"
        }
        files += 1;
        insertions += a.parse::<usize>().unwrap_or(0);
        deletions += d.parse::<usize>().unwrap_or(0);
    }

    Some(Commit {
        hash,
        timestamp: format!("{} {}", date, time),
        weekday,
        hour,
        files,
        insertions,
        deletions,
    })
}

/// Parse the whole log. Returns the commits plus how many records were dropped
/// as unparseable — a repo with odd history should say so rather than lie.
fn parse_log(stdout: &str) -> (Vec<Commit>, usize) {
    let mut commits = Vec::new();
    let mut dropped = 0usize;
    for record in stdout.split('\u{1e}') {
        if record.trim().is_empty() {
            continue;
        }
        match parse_record(record) {
            Some(c) => commits.push(c),
            None => dropped += 1,
        }
    }
    (commits, dropped)
}

fn build_report_with_omissions(commits: &[Commit], dropped: usize) -> Report {
    let mut r = Report {
        total: commits.len(),
        omissions: dropped,
        ..Default::default()
    };
    let mut days: HashMap<String, usize> = HashMap::new();
    let mut day_list: Vec<String> = Vec::new();

    for c in commits {
        r.insertions += c.insertions;
        r.deletions += c.deletions;
        r.by_weekday[c.weekday] += 1;
        r.by_hour[c.hour] += 1;
        let day = c.timestamp.split_whitespace().next().unwrap_or("").to_string();
        if !day.is_empty() {
            if !days.contains_key(&day) {
                day_list.push(day.clone());
            }
            *days.entry(day).or_insert(0) += 1;
        }
    }

    day_list.sort();
    day_list.dedup();
    r.distinct_days = day_list.len();
    r.first_date = day_list.first().cloned().unwrap_or_default();
    r.last_date = day_list.last().cloned().unwrap_or_default();

    // streaks over sorted distinct days
    let mut longest = 0usize;
    let mut run = 0usize;
    let mut prev: Option<i64> = None;
    for day in &day_list {
        let n = day_to_ordinal(day);
        match (prev, n) {
            (Some(p), Some(cur)) if cur == p + 1 => run += 1,
            (_, Some(_)) => run = 1,
            _ => {}
        }
        longest = longest.max(run);
        prev = n.or(prev);
    }
    r.longest_streak = longest;
    r.current_streak = trailing_streak(&day_list);

    let mut top: Vec<(String, usize)> = days.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    r.top_days = top;

    r.files_touched = commits.iter().map(|c| c.files).sum();

    let mut months: HashMap<String, usize> = HashMap::new();
    let mut month_keys: Vec<String> = Vec::new();
    for c in commits {
        if let Some(date) = c.timestamp.split_whitespace().next() {
            let month = date.chars().take(7).collect::<String>();
            if month.len() == 7 {
                if !months.contains_key(&month) {
                    month_keys.push(month.clone());
                }
                *months.entry(month).or_insert(0) += 1;
            }
        }
    }
    month_keys.sort();
    r.months = month_keys
        .into_iter()
        .map(|m| {
            let n = months[&m];
            (m, n)
        })
        .collect();
    let mut heavy: Vec<(String, usize, usize)> = commits
        .iter()
        .map(|c| (c.hash.chars().take(8).collect::<String>(), c.files, c.churn()))
        .collect();
    heavy.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    r.heaviest = heavy;
    r
}

/// Days since the Unix epoch, so consecutive days are consecutive integers.
fn day_to_ordinal(day: &str) -> Option<i64> {
    let mut it = day.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    Some(ymd_to_days(y, m, d))
}

/// Howard Hinnant's days_from_civil, valid for any Gregorian date.
fn ymd_to_days(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn trailing_streak(days: &[String]) -> usize {
    if days.is_empty() {
        return 0;
    }
    let mut run = 1usize;
    for w in days.windows(2).rev() {
        match (day_to_ordinal(&w[0]), day_to_ordinal(&w[1])) {
            (Some(a), Some(b)) if b == a + 1 => run += 1,
            _ => break,
        }
    }
    run
}

fn bar(value: usize, max: usize, width: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let n = ((value as f64 / max as f64) * width as f64).round() as usize;
    "\u{2588}".repeat(n.max(if value > 0 { 1 } else { 0 }))
}

/// Total changed lines across the repository.
fn total_churn(r: &Report) -> usize {
    r.insertions + r.deletions
}

fn render(r: &Report, top_n: usize, graph: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!("commits         {}\n", r.total));
    out.push_str(&format!("distinct days   {}\n", r.distinct_days));
    out.push_str(&format!("range           {} .. {}\n", r.first_date, r.last_date));
    out.push_str(&format!(
        "churn           +{} / -{}  ({} changed lines)\n",
        r.insertions,
        r.deletions,
        total_churn(r)
    ));
    out.push_str(&format!(
        "streak          longest {} days, current {} days\n",
        r.longest_streak, r.current_streak
    ));
    out.push_str(&format!("files touched   {}\n", r.files_touched));
    if r.omissions > 0 {
        out.push_str(&format!(
            "skipped         {} unparseable log record(s)\n",
            r.omissions
        ));
    }

    let names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let max_wd = *r.by_weekday.iter().max().unwrap_or(&0);
    out.push_str("\nby weekday\n");
    for (i, name) in names.iter().enumerate() {
        out.push_str(&format!(
            "  {name}  {:>5}  {}\n",
            r.by_weekday[i],
            bar(r.by_weekday[i], max_wd, 30)
        ));
    }

    if graph {
        let max_h = *r.by_hour.iter().max().unwrap_or(&0);
        out.push_str("\nby hour (local time of each commit)\n");
        for h in 0..24 {
            if r.by_hour[h] == 0 {
                continue;
            }
            out.push_str(&format!(
                "  {:02}:00  {:>5}  {}\n",
                h,
                r.by_hour[h],
                bar(r.by_hour[h], max_h, 30)
            ));
        }
    }

    if !r.months.is_empty() {
        let max_m = r.months.iter().map(|(_, n)| *n).max().unwrap_or(0);
        out.push_str("\nby month\n");
        for (month, n) in &r.months {
            out.push_str(&format!("  {month}  {:>5}  {}\n", n, bar(*n, max_m, 24)));
        }
    }

    if top_n > 0 && !r.top_days.is_empty() {
        out.push_str(&format!("\nbusiest days (top {})\n", top_n.min(r.top_days.len())));
        for (day, n) in r.top_days.iter().take(top_n) {
            out.push_str(&format!("  {day}  {n:>4} commits\n"));
        }
    }

    if top_n > 0 && !r.heaviest.is_empty() {
        out.push_str(&format!(
            "\nheaviest commits (top {})\n",
            top_n.min(r.heaviest.len())
        ));
        for (hash, files, churn) in r.heaviest.iter().take(top_n) {
            out.push_str(&format!(
                "  {hash}  {churn:>7} lines  {files:>4} files\n"
            ));
        }
    }
    out
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn render_json(r: &Report, top_n: usize) -> String {
    let wd: Vec<String> = r.by_weekday.iter().map(|n| n.to_string()).collect();
    let hr: Vec<String> = r.by_hour.iter().map(|n| n.to_string()).collect();
    let days: Vec<String> = r
        .top_days
        .iter()
        .take(top_n)
        .map(|(d, n)| format!("{{\"date\":\"{}\",\"commits\":{}}}", json_escape(d), n))
        .collect();
    format!(
        "{{\n  \"commits\": {},\n  \"distinct_days\": {},\n  \"first_date\": \"{}\",\n  \
         \"last_date\": \"{}\",\n  \"insertions\": {},\n  \"deletions\": {},\n  \
         \"total_churn\": {},\n  \"files_touched\": {},\n  \"omitted_records\": {},\n  \"longest_streak\": {},\n  \
         \"current_streak\": {},\n  \"by_weekday\": [{}],\n  \"by_hour\": [{}],\n  \
         \"busiest_days\": [{}],\n  \"heaviest_commits\": [{}],\n  \"months\": {{{}}}\n}}",
        r.total,
        r.distinct_days,
        json_escape(&r.first_date),
        json_escape(&r.last_date),
        r.insertions,
        r.deletions,
        total_churn(r),
        r.files_touched,
        r.omissions,
        r.longest_streak,
        r.current_streak,
        wd.join(","),
        hr.join(","),
        days.join(","),
        heavy_json(r, top_n),
        months_json(r)
    )
}

/// Per-month commit counts as a JSON object.
fn months_json(r: &Report) -> String {
    r.months
        .iter()
        .map(|(m, n)| format!("\"{}\":{}", json_escape(m), n))
        .collect::<Vec<_>>()
        .join(",")
}

/// Heaviest commits as a JSON array.
fn heavy_json(r: &Report, top_n: usize) -> String {
    r.heaviest
        .iter()
        .take(top_n)
        .map(|(h, f, c)| {
            format!(
                "{{\"hash\":\"{}\",\"files\":{},\"churn\":{}}}",
                json_escape(h),
                f,
                c
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn run_git_log(path: &str, since: Option<&str>, author: Option<&str>) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(path);
    cmd.args([
        "log",
        "--no-merges",
        // %u is the ISO weekday number, so results do not depend on the user's locale.
        "--date=format-local:%u|%Y-%m-%d|%H:%M:%S",
        "--pretty=format:\u{1e}%H\u{1f}%cd",
        "--numstat",
    ]);
    if let Some(s) = since {
        cmd.arg(format!("--since={s}"));
    }
    if let Some(a) = author {
        cmd.arg(format!("--author={a}"));
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let out = cmd.output().map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn main() {
    let mut args: Vec<String> = env::args().skip(1).collect();
    let mut since: Option<String> = None;
    let mut author: Option<String> = None;
    let mut top_n = 5usize;
    let mut json = false;
    let mut graph = true;
    let mut path = ".".to_string();

    let mut i = 0;
    while i < args.len() {
        let a = args[i].clone();
        match a.as_str() {
            "-h" | "--help" => {
                println!("{}", usage());
                return;
            }
            "-V" | "--version" => {
                println!("gitpulse {VERSION}");
                return;
            }
            "--json" => json = true,
            "--no-graph" => graph = false,
            "--since" => {
                i += 1;
                match args.get(i) {
                    Some(v) => since = Some(v.clone()),
                    None => {
                        eprintln!("gitpulse: --since needs a value");
                        std::process::exit(2);
                    }
                }
            }
            "--author" => {
                i += 1;
                match args.get(i) {
                    Some(v) => author = Some(v.clone()),
                    None => {
                        eprintln!("gitpulse: --author needs a value");
                        std::process::exit(2);
                    }
                }
            }
            "--top" => {
                i += 1;
                match args.get(i).and_then(|v| v.parse().ok()) {
                    Some(v) => top_n = v,
                    None => {
                        eprintln!("gitpulse: --top needs a number");
                        std::process::exit(2);
                    }
                }
            }
            other if other.starts_with('-') => {
                eprintln!("gitpulse: unknown option {other}\n\n{}", usage());
                std::process::exit(2);
            }
            other => path = other.to_string(),
        }
        i += 1;
    }
    args.clear();

    let stdout = match run_git_log(&path, since.as_deref(), author.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("gitpulse: git log failed: {e}");
            std::process::exit(1);
        }
    };

    let (commits, dropped) = parse_log(&stdout);
    if commits.is_empty() {
        println!("no commits found in {path}");
        return;
    }
    let report = build_report_with_omissions(&commits, dropped);
    if json {
        println!("{}", render_json(&report, top_n));
    } else {
        print!("{}", render(&report, top_n, graph));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Convenience wrapper for tests that do not care about dropped records.
    fn build_report(commits: &[Commit]) -> Report {
        build_report_with_omissions(commits, 0)
    }

    #[test]
    fn parses_a_single_record() {
        // Real shape emitted by `git log --pretty=\x1e%H\x1f... --numstat`.
        let raw = "\u{1e}abc123\u{1f}7|2026-11-15|14:03:22\n10\t2\tsrc/main.rs\n-\t-\timage.png";
        let c = parse_record(raw).expect("record");
        assert_eq!(c.hash, "abc123");
        assert_eq!(c.weekday, 6); // ISO 7 == Sunday, stored as 6 from Monday
        assert_eq!(c.hour, 14);
        assert_eq!(c.files, 1); // binary file skipped
        assert_eq!(c.insertions, 10);
        assert_eq!(c.deletions, 2);
        assert_eq!(c.churn(), 12);
    }

    #[test]
    fn parses_multiple_records() {
        let raw = "\u{1e}a\u{1f}1|2026-01-05|09:00:00\n1\t0\tf\
                   \u{1e}b\u{1f}2|2026-01-06|10:00:00\n0\t1\tg";
        let (commits, dropped) = parse_log(raw);
        assert_eq!(commits.len(), 2);
        assert_eq!(dropped, 0);
        assert_eq!(commits[1].weekday, 1);
        assert_eq!(commits[1].hour, 10);
        assert_eq!(commits[1].insertions, 0);
        assert_eq!(commits[1].deletions, 1);
    }

    #[test]
    fn skips_records_without_hash() {
        let (commits, dropped) = parse_log("\u{1e}\u{1f}1|2026-01-05|09:00:00");
        assert!(commits.is_empty());
        assert_eq!(dropped, 1);
    }

    #[test]
    fn sums_churn_across_files() {
        let raw = "\u{1e}h\u{1f}3|2026-02-10|08:30:00\n5\t1\ta.rs\n7\t2\tb.rs";
        let c = parse_record(raw).expect("record");
        assert_eq!(c.files, 2);
        assert_eq!(c.insertions, 12);
        assert_eq!(c.deletions, 3);
    }

    #[test]
    fn weekday_mapping() {
        assert_eq!(iso_weekday_index("1"), 0); // Monday
        assert_eq!(iso_weekday_index("5"), 4); // Friday
        assert_eq!(iso_weekday_index("7"), 6); // Sunday
        assert_eq!(iso_weekday_index(""), 0);  // unknown falls back, never panics
        assert_eq!(iso_weekday_index("9"), 0);
    }

    #[test]
    fn day_ordinals_are_consecutive() {
        assert_eq!(day_to_ordinal("2026-01-01"), Some(day_to_ordinal("2025-12-31").unwrap() + 1));
        assert_eq!(day_to_ordinal("2024-03-01"), Some(day_to_ordinal("2024-02-29").unwrap() + 1));
    }

    #[test]
    fn builds_streaks() {
        let days: Vec<String> = ["2026-01-01", "2026-01-02", "2026-01-03", "2026-01-09"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(trailing_streak(&days), 1);
        let commits: Vec<Commit> = days
            .iter()
            .map(|d| Commit {
                hash: "x".into(),
                timestamp: format!("{d} 12:00:00"),
                weekday: 0,
                hour: 12,
                files: 1,
                insertions: 1,
                deletions: 0,
            })
            .collect();
        let r = build_report(&commits);
        assert_eq!(r.distinct_days, 4);
        assert_eq!(r.longest_streak, 3);
        assert_eq!(r.current_streak, 1);
    }

    #[test]
    fn empty_report_renders() {
        let r = Report::default();
        let out = render(&r, 5, true);
        assert!(out.contains("commits         0"));
    }

    #[test]
    fn reports_dropped_records_instead_of_hiding_them() {
        let raw = "\u{1e}h1\u{1f}1|2026-01-05|09:00:00\n1\t0\ta\u{1e}\u{1f}garbage";
        let (commits, dropped) = parse_log(raw);
        assert_eq!(commits.len(), 1);
        assert_eq!(dropped, 1);
        let r = build_report_with_omissions(&commits, dropped);
        assert_eq!(r.omissions, 1);
        assert!(render(&r, 0, false).contains("skipped         1 unparseable"));
        assert!(render_json(&r, 0).contains("\"omitted_records\": 1"));
    }

    #[test]
    fn groups_commits_by_month() {
        let mk = |ts: &str| Commit {
            hash: "h".into(),
            timestamp: ts.into(),
            weekday: 1,
            hour: 9,
            files: 1,
            insertions: 1,
            deletions: 0,
        };
        let commits = vec![
            mk("2026-01-10 09:00:00"),
            mk("2026-01-20 09:00:00"),
            mk("2026-02-01 09:00:00"),
        ];
        let r = build_report(&commits);
        assert_eq!(r.months, vec![("2026-01".to_string(), 2), ("2026-02".to_string(), 1)]);
        assert!(render_json(&r, 0).contains("\"months\": {\"2026-01\":2,\"2026-02\":1}"));
    }

    #[test]
    fn ranks_heaviest_commits_and_counts_files() {
        let mk = |hash: &str, files: usize, ins: usize, del: usize| Commit {
            hash: hash.into(),
            timestamp: "2026-03-03 10:00:00".into(),
            weekday: 1,
            hour: 10,
            files,
            insertions: ins,
            deletions: del,
        };
        let commits = vec![mk("aaaa1111", 2, 10, 0), mk("bbbb2222", 1, 500, 200)];
        let r = build_report(&commits);
        assert_eq!(r.files_touched, 3);
        assert_eq!(r.heaviest[0].0, "bbbb2222"); // biggest churn first
        assert_eq!(r.heaviest[0].2, 700);
        assert_eq!(r.heaviest[1].0, "aaaa1111");
        assert!(render_json(&r, 1).contains("\"heaviest_commits\": [{\"hash\":\"bbbb2222\""));
    }

    #[test]
    fn json_is_well_formed_shape() {
        let commits = vec![Commit {
            hash: "h".into(),
            timestamp: "2026-05-05 08:00:00".into(),
            weekday: 1,
            hour: 8,
            files: 2,
            insertions: 30,
            deletions: 4,
        }];
        let r = build_report(&commits);
        let j = render_json(&r, 3);
        assert!(j.starts_with('{'));
        assert!(j.contains("\"commits\": 1"));
        assert!(j.contains("\"insertions\": 30"));
        assert!(j.contains("\"busiest_days\": [{\"date\":\"2026-05-05\""));
    }
}
