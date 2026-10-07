#![no_std]
#![no_main]

use sydon_rt::director::{self, JobState};
use sydon_rt::rpc::Error;
use sydon_rt::{format, print, println, sys, String, Vec};

sydon_rt::main!(main);

const HELP: &str = "\
commands:
  help                     this list
  jobs                     programs you can start
  <job>                    run a job and wait for it (ctrl-c stops it)
  run <job> [--on <cpu>]   the same, on a chosen cpu
  spawn <job> [--on <cpu>] start a job in the background
  wait <pid>               wait for a background job
  kill <pid>               end a job
  ps                       jobs across all cores
  services                 registered services and where they live
  cores                    cores, their load and heartbeats
  hang <cpu>               stop a core, to watch the director notice
  uptime                   time since boot
  clear                    clear the screen
  poweroff                 shut down";

fn main() -> u64 {
    println!("sydonOS shell on cpu {}. type help", sys::info().0);
    loop {
        print!("sydon> ");
        let Some(line) = sydon_rt::io::read_line() else {
            println!();
            continue;
        };
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.is_empty() || builtin(&words) {
            continue;
        }
        match words[0] {
            "run" | "spawn" => match job_args(&words[1..]) {
                Some((name, core)) => start(name, core, words[0] == "spawn"),
                None => println!("usage: {} <job> [--on <cpu>]", words[0]),
            },
            _ => match job_args(&words) {
                Some((name, core)) => start(name, core, false),
                None => println!("usage: <job> [--on <cpu>]"),
            },
        }
    }
}

fn job_args<'a>(words: &[&'a str]) -> Option<(&'a str, Option<usize>)> {
    match words {
        [name] => Some((name, None)),
        [name, "--on", cpu] => Some((name, Some(cpu.parse().ok()?))),
        _ => None,
    }
}

// a job run in the foreground can be stopped with ctrl-c
fn start(name: &str, core: Option<usize>, background: bool) {
    let (pid, cpu) = match director::spawn(name, core) {
        Ok(started) => started,
        Err(Error::NotFound) => return println!("{}: not found, see jobs", name),
        Err(e) => return println!("{}: {}", name, e),
    };
    if background {
        return println!("started {} as pid {} on cpu {}", name, pid, cpu);
    }
    let _ = director::foreground(&[pid]);
    let result = director::wait(pid);
    let _ = director::foreground(&[]);
    match result {
        Ok(0) => {}
        other => report(pid, name, other),
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
        "ps" => ps(),
        "services" => services(),
        "cores" => cores(),
        "hang" => match num() {
            Some(core) => match director::hang(core as usize) {
                Ok(()) => println!("cpu {} told to hang", core),
                Err(e) => println!("hang: {}", e),
            },
            None => println!("usage: hang <cpu>"),
        },
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

fn report(pid: u64, name: &str, result: Result<u64, Error>) {
    let who = if name.is_empty() { format!("pid {}", pid) } else { format!("{} (pid {})", name, pid) };
    match result {
        Ok(director::KILLED) => println!("{} was killed", who),
        Ok(code) => println!("{} exited with {}", who, code),
        Err(e) => println!("{}: {}", who, e),
    }
}

fn jobs() {
    match director::images() {
        Ok(list) => {
            println!("  NAME       NEEDS");
            for img in list {
                println!("  {:<9}  {}", img.name, img.needs);
            }
        }
        Err(e) => println!("jobs: {}", e),
    }
}

fn ps() {
    let list = match director::ps() {
        Ok(list) => list,
        Err(e) => return println!("ps: {}", e),
    };
    println!("  PID  CPU  STATE      NAME");
    for j in list {
        let state: String = match j.state {
            JobState::Starting => "starting".into(),
            JobState::Running => "running".into(),
            JobState::Exited(code) => format!("exit {}", code),
            JobState::Failed => "failed".into(),
            JobState::Lost => "lost".into(),
        };
        println!("{:>5}  {:>3}  {:<9}  {}", j.pid, j.core, state, j.name);
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
            println!("  CPU  STATE  JOBS  FREE MEM  LAST HEARTBEAT");
            for c in list {
                let beat = if c.core == 0 { String::from("-") } else { format!("{} ms ago", c.silent_ms) };
                let state = if c.up { "up" } else { "down" };
                println!("  {:>3}  {:<5}  {:>4}  {:>5} MiB  {}", c.core, state, c.jobs, c.free_kib / 1024, beat);
            }
        }
        Err(e) => println!("cores: {}", e),
    }
}
