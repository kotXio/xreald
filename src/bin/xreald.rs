// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Kostiantyn Andriiuk <konstantin@andriyuk.com>
fn main() {
    let result = (|| -> Result<(), String> {
        let Some(config) = xreald::daemon::Config::arguments(std::env::args().skip(1))? else {
            return Ok(());
        };
        let mut server = xreald::daemon::Server::new(config).map_err(|e| e.to_string())?;
        server.run().map_err(|e| e.to_string())
    })();
    if let Err(error) = result {
        eprintln!("xreald: {error}");
        std::process::exit(1);
    }
}
