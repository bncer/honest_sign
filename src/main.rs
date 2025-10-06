extern crate google_sheets4 as sheets4;
extern crate hyper;
extern crate hyper_rustls;
use rustls::crypto::CryptoProvider;
use sheets4::{Result, Sheets, yup_oauth2};
use dotenv::dotenv;
use std::env;

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();

    CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider())
        .expect("Failed to install default crypto provider");

    let sa_key_path = env::var("GOOGLE_CREDENTIALS").expect("Json file with Google credentials must be in .env file");
    let sa_key = yup_oauth2::read_service_account_key(sa_key_path)
        .await
        .expect("Couldn't read service account key");

    let auth = yup_oauth2::ServiceAccountAuthenticator::builder(sa_key)
        .build()
        .await
        .expect("Failed to create authenticator");

    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build(
            hyper_rustls::HttpsConnectorBuilder::new()
                .with_native_roots()
                .unwrap()
                .https_or_http()
                .enable_http1()
                .build(),
        );

    let hub = Sheets::new(client, auth);

    let spreadsheet_id = env::var("GOOGLE_SHEET_ID").expect("Google Sheets ID must be in .env file");
    let range = "Sheet1!A1:C10";

    let result = hub
        .spreadsheets()
        .values_get(&spreadsheet_id, range)
        .doit()
        .await;

    match result {
        Ok((_response, value_range)) => {
            if let Some(values) = value_range.values {
                println!("Successfully retrieved data:");
                for row in values {
                    for cell in row {
                        print!("{}\t", cell);
                    }
                    println!();
                }
            } else {
                println!("No data found in the specified range.");
            }
        }
        Err(e) => {
            eprintln!("Error retrieving data: {}", e);
        }
    }

    Ok(())
}
