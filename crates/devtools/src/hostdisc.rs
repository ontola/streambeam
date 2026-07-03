//! Browse for StreamBeam hosts on the LAN via mDNS.

use std::time::Duration;

use clap::Parser;
use streambeam_net::browse;

#[derive(Parser, Debug)]
#[command(name = "hostdisc")]
struct Opt {
    /// Browse duration in seconds.
    #[arg(long, default_value_t = 3)]
    timeout: u64,
}

fn main() {
    let opt = Opt::parse();
    match browse(Duration::from_secs(opt.timeout)) {
        Ok(hosts) if hosts.is_empty() => {
            eprintln!("no StreamBeam hosts found (is fakehost running with mDNS?)");
            std::process::exit(1);
        }
        Ok(hosts) => {
            for h in hosts {
                println!(
                    "{}  port {}  addrs {:?}",
                    h.instance.trim_end_matches('.'),
                    h.port,
                    h.addrs
                );
            }
        }
        Err(e) => {
            eprintln!("hostdisc error: {e}");
            std::process::exit(1);
        }
    }
}
