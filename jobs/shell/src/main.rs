#![no_std]
#![no_main]

use sydon_rt::director::{self, JobState};
use sydon_rt::io::{self, Stream, BROKEN_PIPE};
use sydon_rt::rpc::Error;
use sydon_rt::{format, print, println, sys, String, ToString, Vec};

sydon_rt::main!(main);

const HELP: &str = "\
commands:
  help                     this list
  jobs                     programs you can start, with their class
  <prog> [args]            run a program from /bin and wait for it
  a | b | c                a pipeline. any stage can end in --on <cpu>
  <prog> < <file>          read input from a file
  <prog> > <file>          write output to a file, >> adds to its end
  <command> &              start in the background
  time <command>           run and say how long it took
  run <job> [--on <cpu>]   same as just naming the job
  spawn <job> [--on <cpu>] start a job in the background
  wait <pid>               wait for a background job
  kill <pid>               end a job (ctrl-c ends the ones in front)
  ls [dir]                 list a directory
  ps                       jobs across all cores
  services                 registered services and where they live
  cores                    cores, their load and heartbeats
  hang <cpu>               stop a core, to watch the director notice
  crash <cpu>              make a core's kernel panic, to watch it come back
  poll <cpu|all> <mode>    always, adaptive or never: how an idle core waits
  park <cpu>               no new jobs there, so its sibling thread has the core
  unpark <cpu>             take a parked core back
  stats                    per-core event counters
  sync                     make sure written files are on the disk
  uptime                   time since boot
  clear                    clear the screen
  poweroff                 shut down
programs in /bin include echo cat grep wc head tail yes seq sort
touch rm mkdir cp tee";

enum Tok {
    Word(String),
    Pipe,
    From,
    To(bool),
    Background,
}

fn tokens(line: &str) -> Result<Vec<Tok>, &'static str> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        let special = match c {
            '|' => Some(Tok::Pipe),
            '<' => Some(Tok::From),
            '>' => Some(Tok::To(false)),
            '&' => Some(Tok::Background),
            _ => None,
        };
        if let Some(mut t) = special {
            chars.next();
            if matches!(t, Tok::To(_)) && chars.peek() == Some(&'>') {
                chars.next();
                t = Tok::To(true);
            }
            out.push(t);
            continue;
        }
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        let mut word = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() || "|<>&".contains(c) {
                break;
            }
            chars.next();
            if c == '\'' || c == '"' {
                loop {
                    match chars.next() {
                        Some(q) if q == c => break,
                        Some(x) => word.push(x),
                        None => return Err("missing closing quote"),
                    }
                }
            } else {
                word.push(c);
            }
        }
        out.push(Tok::Word(word));
    }
    Ok(out)
}

struct Stage {
    words: Vec<String>,
    core: Option<usize>,
}

struct Command {
    stages: Vec<Stage>,
    input: Option<String>,
    output: Option<(String, bool)>,
    background: bool,
    timed: bool,
}

fn parse(line: &str) -> Result<Option<Command>, String> {
    let mut cmd = Command { stages: Vec::new(), input: None, output: None, background: false, timed: false };
    let mut words = Vec::new();
    let mut toks = tokens(line)?.into_iter().peekable();
    while let Some(t) = toks.next() {
        match t {
            Tok::Word(w) => words.push(w),
            Tok::Pipe if cmd.output.is_some() => return Err("> goes on the last program".into()),
            Tok::Pipe => cmd.stages.push(stage(core::mem::take(&mut words))?),
            Tok::From => match toks.next() {
                Some(Tok::Word(path)) if cmd.stages.is_empty() && cmd.input.is_none() => cmd.input = Some(path),
                _ => return Err("< takes one file, for the first program".into()),
            },
            Tok::To(append) => match toks.next() {
                Some(Tok::Word(path)) if cmd.output.is_none() => cmd.output = Some((path, append)),
                _ => return Err("> takes one file".into()),
            },
            Tok::Background if toks.peek().is_none() => cmd.background = true,
            Tok::Background => return Err("& goes at the end".into()),
        }
    }
    if words.is_empty() && cmd.stages.is_empty() && cmd.input.is_none() && cmd.output.is_none() {
        return Ok(None);
    }
    cmd.stages.push(stage(words)?);
    if cmd.stages[0].words[0] == "time" {
        cmd.stages[0].words.remove(0);
        cmd.timed = true;
        if cmd.stages[0].words.is_empty() {
            return Err("time what?".into());
        }
    }
    Ok(Some(cmd))
}

fn stage(mut words: Vec<String>) -> Result<Stage, String> {
    let mut core = None;
    let n = words.len();
    if n >= 2 && words[n - 2] == "--on" {
        core = Some(words[n - 1].parse().map_err(|_| format!("--on {}: not a cpu", words[n - 1]))?);
        words.truncate(n - 2);
    }
    if words.is_empty() {
        return Err("empty command".into());
    }
    Ok(Stage { words, core })
}

fn main() -> u64 {
    println!("sydonOS shell on cpu {}. type help", sys::info().0);
    loop {
        print!("sydon> ");
        let Some(line) = io::read_line() else {
            println!();
            sys::sleep_ns(10_000_000);
            continue;
        };
        match parse(&line) {
            Ok(Some(cmd)) => run(cmd),
            Ok(None) => {}
            Err(e) => println!("shell: {}", e),
        }
    }
}

fn run(mut cmd: Command) {
    let started = sys::time_ns();
    let simple = cmd.stages.len() == 1 && cmd.input.is_none() && cmd.output.is_none() && !cmd.background;
    let words: Vec<&str> = cmd.stages[0].words.iter().map(|w| w.as_str()).collect();
    if simple && builtin(&words) {
    } else if simple && matches!(words[0], "run" | "spawn") {
        let background = words[0] == "spawn";
        match words[1..] {
            [name] => {
                cmd.stages[0].words = [name.to_string()].into();
                cmd.background = background;
                execute(&cmd);
            }
            _ => println!("usage: {} <job> [--on <cpu>]", words[0]),
        }
    } else {
        execute(&cmd);
    }
    if cmd.timed {
        let us = (sys::time_ns() - started) / 1000;
        println!("real {}.{:03} ms", us / 1000, us % 1000);
    }
}

fn builtin(words: &[&str]) -> bool {
    let (cmd, args) = (words[0], &words[1..]);
    let num = || args.first().and_then(|a| a.parse::<u64>().ok());
    match cmd {
        "help" => println!("{}", HELP),
        "jobs" => jobs(),
        "wait" => match num() {
            Some(pid) => report(pid, "", director::wait(pid)),
            None => println!("usage: wait <pid>"),
        },
        "kill" => match num() {
            Some(pid) => {
                if let Err(e) = director::kill(pid) {
                    println!("kill: {}", e);
                }
            }
            None => println!("usage: kill <pid>"),
        },
        "ls" => ls(args.first().copied().unwrap_or("/")),
        "ps" => ps(),
        "stats" => stats(),
        "services" => services(),
        "cores" => cores(),
        "hang" | "crash" => match num() {
            Some(core) => match if cmd == "hang" { director::hang(core as usize) } else { director::crash(core as usize) } {
                Ok(()) => println!("cpu {} told to {}", core, cmd),
                Err(e) => println!("{}: {}", cmd, e),
            },
            None => println!("usage: {} <cpu>", cmd),
        },
        "park" | "unpark" => match num() {
            Some(core) => {
                if let Err(e) = director::park(core as usize, cmd == "park") {
                    println!("{}: {}", cmd, e);
                }
            }
            None => println!("usage: {} <cpu>", cmd),
        },
        "poll" => {
            let mode = match args.get(1).copied() {
                Some("always") => Some(0),
                Some("adaptive") => Some(1),
                Some("never") => Some(2),
                _ => None,
            };
            let cores = director::cores().map(|c| c.len()).unwrap_or(0);
            let which: Vec<usize> = match args.first().copied() {
                Some("all") => (0..cores).collect(),
                Some(c) => c.parse().ok().into_iter().collect(),
                None => Vec::new(),
            };
            match mode {
                Some(m) if !which.is_empty() => {
                    for c in which {
                        if let Err(e) = director::poll(c, m) {
                            println!("poll: cpu {}: {}", c, e);
                        }
                    }
                }
                _ => println!("usage: poll <cpu|all> <always|adaptive|never>"),
            }
        }
        "sync" => {
            if let Err(e) = sydon_rt::fs::sync() {
                println!("sync: {}", e);
            }
        }
        "uptime" => {
            let ms = sys::time_ns() / 1_000_000;
            println!("up {}.{:03} s", ms / 1000, ms % 1000);
        }
        "clear" => print!("\x1b[2J\x1b[H"),
        "poweroff" => {
            if let Err(e) = director::poweroff() {
                println!("poweroff: {}", e);
            }
        }
        _ => return false,
    }
    true
}

fn execute(cmd: &Command) {
    let input = match &cmd.input {
        Some(path) => match sydon_rt::fs::open(path) {
            Ok(f) if f.is_dir() => return println!("{}: is a directory", path),
            Ok(f) => {
                let (h, ino) = f.raw();
                Stream::File(h, ino)
            }
            Err(e) => return println!("{}: {}", path, e),
        },
        None if cmd.background => Stream::Null,
        None => Stream::Console,
    };
    let mut out = match &cmd.output {
        Some((path, append)) => match open_output(path, *append) {
            Ok(s) => s,
            Err(e) => return println!("{}: {}", path, e),
        },
        None => Stream::Console,
    };
    let mut jobs: Vec<(u64, usize, &str)> = Vec::new();
    for (i, stage) in cmd.stages.iter().enumerate().rev() {
        let name = stage.words[0].as_str();
        let args: Vec<&str> = stage.words.iter().map(|w| w.as_str()).collect();
        let stdin = if i == 0 { input } else { Stream::NewPipe };
        match director::spawn_with(name, stage.core, [stdin, out, Stream::Console], &args) {
            Ok(s) => {
                jobs.push((s.pid, s.core, name));
                out = s.input.unwrap_or(Stream::Null);
            }
            Err(e) => {
                println!("{}: {}", name, e);
                for (pid, _, _) in jobs {
                    let _ = director::kill(pid);
                }
                return;
            }
        }
    }
    jobs.reverse();
    if cmd.background {
        for (pid, core, name) in jobs {
            println!("started {} as pid {} on cpu {}", name, pid, core);
        }
        return;
    }
    let pids: Vec<u64> = jobs.iter().map(|j| j.0).collect();
    let _ = io::set_foreground(&pids);
    let last = jobs.len() - 1;
    for (i, &(pid, _, name)) in jobs.iter().enumerate() {
        let result = director::wait(pid);
        let quiet = match result {
            // 1 is an ordinary failure, the program has said why
            Ok(0 | 1) => true,
            // the stage after it finished first
            Ok(BROKEN_PIPE) | Ok(director::KILLED) => i != last,
            _ => false,
        };
        if !quiet {
            report(pid, name, result);
        }
    }
    let _ = io::set_foreground(&[]);
}

fn open_output(path: &str, append: bool) -> Result<Stream, Error> {
    let f = sydon_rt::fs::create(path)?;
    if f.is_dir() {
        return Err(Error::Invalid);
    }
    if !append {
        f.truncate(0)?;
    }
    let (h, ino) = f.raw();
    Ok(Stream::FileAppend(h, ino))
}

fn report(pid: u64, name: &str, result: Result<u64, Error>) {
    let who = if name.is_empty() { format!("pid {}", pid) } else { format!("{} (pid {})", name, pid) };
    match result {
        Ok(director::KILLED) => println!("{} was killed", who),
        Ok(code) => println!("{} exited with {}", who, code),
        Err(e) => println!("{}: {}", who, e),
    }
}

fn jobs() {
    println!("  NAME       CLASS       NEEDS     FROM");
    if let Ok(list) = director::images() {
        for img in list {
            println!("  {:<9}  {:<10}  {:<8}  built in", img.name, "other", img.needs);
        }
    }
    match sydon_rt::fs::read_dir("/bin") {
        Ok(entries) => {
            for e in entries.iter().filter(|e| !e.dir) {
                let (class, needs) = sydon_rt::fs::manifest(&sydon_rt::format!("/bin/{}", e.name))
                    .unwrap_or(("other", sydon_rt::String::new()));
                println!("  {:<9}  {:<10}  {:<8}  /bin", e.name, class, needs);
            }
        }
        Err(e) => println!("jobs: /bin: {}", e),
    }
}

fn ps() {
    let list = match director::ps() {
        Ok(list) => list,
        Err(e) => return println!("ps: {}", e),
    };
    println!("  PID  CPU  STATE      NAME");
    for j in list {
        let state = match j.state {
            JobState::Starting => "starting".into(),
            JobState::Running => "running".into(),
            JobState::Exited(code) => sydon_rt::format!("exit {}", code),
            JobState::Failed => "failed".into(),
            JobState::Lost => "lost".into(),
        };
        println!("{:>5}  {:>3}  {:<9}  {}", j.pid, j.core, state, j.name);
    }
}

fn ls(path: &str) {
    match sydon_rt::fs::read_dir(path) {
        Ok(entries) => {
            for e in entries {
                if e.dir {
                    println!("  {}/", e.name);
                } else {
                    println!("  {:<24} {:>10}", e.name, e.size);
                }
            }
        }
        Err(e) => println!("ls: {}: {}", path, e),
    }
}

fn stats() {
    let cores = director::cores().map(|c| c.len()).unwrap_or(0);
    println!("  CPU  DOORBELLS OUT   IN     HALTS   SYSCALLS   MSGS IN");
    for c in 0..cores {
        match director::stats(c) {
            Ok([out, inn, halts, sys, msgs]) => {
                println!("  {:>3}  {:>13} {:>5} {:>9} {:>10} {:>9}", c, out, inn, halts, sys, msgs)
            }
            Err(e) => println!("  {:>3}  {}", c, e),
        }
    }
}

fn services() {
    match director::services() {
        Ok(list) => {
            println!("  NAME        CPU  HANDLE");
            for (name, svc) in list {
                println!("  {:<10}  {:>3}  {:#x}", name, svc.core(), svc.0);
            }
        }
        Err(e) => println!("services: {}", e),
    }
}

fn cores() {
    match director::cores() {
        Ok(list) => {
            println!("  CPU  CORE  L3  STATE   JOBS  FREE MEM  LAST HEARTBEAT");
            for c in list {
                let beat = if c.core == 0 {
                    sydon_rt::String::from("-")
                } else {
                    sydon_rt::format!("{} ms ago", c.silent_ms)
                };
                let state = if !c.up { "down" } else if c.parked { "parked" } else { "up" };
                println!("  {:>3}  {:>4}  {:>2}  {:<6}  {:>4}  {:>5} MiB  {}", c.core, c.phys, c.cluster, state, c.jobs, c.free_kib / 1024, beat);
            }
        }
        Err(e) => println!("cores: {}", e),
    }
}
