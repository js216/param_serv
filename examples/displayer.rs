use param_serv::Connection;
use std::thread::sleep;
use std::time::Duration;

fn main() -> std::io::Result<()> {
    let mut c = Connection::new()?;

    loop {
        print!("\x1b[2J\x1b[H"); // clear screen
        for (name, val) in c.get()? {
            println!("{}: {}", name, val);
        }

        sleep(Duration::from_secs_f64(0.016));
    }
}
