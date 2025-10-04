extern crate google_sheets4 as sheets4;
extern crate hyper;
extern crate hyper_rustls;
use rustls::crypto::CryptoProvider;
use sheets4::{Result, Sheets, yup_oauth2};

#[tokio::main]
async fn main() -> Result<()> {
    CryptoProvider::install_default(rustls::crypto::ring::default_provider())
        .expect("Failed to install default crypto provider");
    // 1. Path to your service account key file
    let sa_key_path = "./credentials.json";
    let sa_key = yup_oauth2::read_service_account_key(sa_key_path)
        .await
        .expect("Couldn't read service account key");

    // 2. Create an authenticator
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

    // 4. Create the Sheets Hub
    let hub = Sheets::new(client, auth);

    // 5. Define your spreadsheet ID and the range to read
    let spreadsheet_id = "1diOg_IPD11a36e9PiQOP-pTOdNM-qMowrhkKPR-z8pQ";
    let range = "Sheet1!A1:C10";

    // 6. Make the API call to get values
    let result = hub
        .spreadsheets()
        .values_get(spreadsheet_id, range)
        .doit()
        .await;

    // 7. Handle the response
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
