use param_serv::Connection;
use std::thread::sleep;
use std::time::Duration;

fn main() -> std::io::Result<()> {
    let mut c = Connection::new()?;
    let names = c.list()?;

    loop {
        print!("\x1b[2J\x1b[H"); // clear screen
        for (index, val) in c.get()? {
            println!("{}: {}", names[index as usize], val);
        }

        sleep(Duration::from_secs_f64(0.016));
    }
}
