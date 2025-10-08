extern crate google_sheets4 as sheets4;
extern crate hyper;
extern crate hyper_rustls;
use rustls::crypto::CryptoProvider;
use sheets4::{Result, Sheets, yup_oauth2};
use dotenv::dotenv;
use std::env;
use chrono::{NaiveDate, Days};

fn sheets_serial_to_date(serial: f64) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(1899, 12, 30)?
        .checked_add_days(Days::new(serial.trunc() as u64))
}

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
    let range = "Sheet1!A1:ZZ10";

    let result = hub
        .spreadsheets()
        .values_get(&spreadsheet_id, range)
        .value_render_option("UNFORMATTED_VALUE")
        .doit().await?;

    if let Some(values) = result.1.values {
        if let Some(date_values) = values.get(0) {
            for i in 1..date_values.len() { 
                if let Some(serial_number) = date_values[i].as_f64() {
                    if let Some(date) = sheets_serial_to_date(serial_number) {
                        println!("Converted Date: {0}, {1}", i, date); 
                    } else {
                        println!("Failed to convert serial number.");
                    }
                }
            }
        }
    }
    Ok(())
}
