extern crate google_sheets4 as sheets4;
extern crate hyper;
extern crate hyper_rustls;
use rustls::crypto::CryptoProvider;
use sheets4::{Result, Sheets, api::BatchGetValuesResponse, yup_oauth2};
use dotenv::dotenv;
use std::env;
use std::path::{Path, PathBuf};
use chrono::{NaiveDate, Days};
use lopdf::{Document, Object, Dictionary};

#[allow(dead_code)]
#[derive(Debug)]
struct Order {
    date: NaiveDate,
    model: String,
    items: Vec<SizeQuantity>,
}

#[allow(dead_code)]
#[derive(Debug)]
struct SizeQuantity {
    size: i64,
    quantity: i64,
}

fn sheets_serial_to_date(serial: f64) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(1899, 12, 30)?
        .checked_add_days(Days::new(serial.trunc() as u64))
}

async fn get_sheet_values(
    sa_key_path: &str,
    spreadsheet_id: &str,
    ranges: &[&str]
) -> BatchGetValuesResponse {
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

    let Ok((_response, data)) = hub
        .spreadsheets()
        .values_batch_get(&spreadsheet_id)
        .add_ranges(&ranges[0])
        .add_ranges(&ranges[1])
        .value_render_option("UNFORMATTED_VALUE")
        .doit().await else { panic!("Cannot get data from Google sheets") };

    data
}

fn parse_order(date_str: &str, response: BatchGetValuesResponse) -> Vec<Order> {
    if let Some(value_ranges) = response.value_ranges {

        let first_range_data = &value_ranges[0].values.as_ref().unwrap();

        let dates: Vec<NaiveDate> = first_range_data[0].iter().skip(1)
            .filter_map(|v| v.as_f64().and_then(sheets_serial_to_date))
            .collect();

        let models: Vec<String> = first_range_data[1].iter().skip(1)
            .filter_map(|v| v.as_str().map(String::from))
            .collect();


        let num_orders = dates.len();
        let mut parsed_orders: Vec<Order> = Vec::with_capacity(num_orders);
        for i in 0..num_orders {
            parsed_orders.push(Order { date: dates[i], model: models[i].clone(), items: Vec::new() });
        }

        let second_range_data = &value_ranges[1].values.as_ref().unwrap();

        for row in second_range_data.iter().skip(2) {
            let size = row.get(0).and_then(|v| v.as_i64()).unwrap_or(0);
            if size == 0 { continue; }
            for col_index in 0..num_orders {
                if let Some(quantity) = row.get(col_index + 1).and_then(|v| v.as_i64()) {
                    if quantity > 0 {
                        parsed_orders[col_index].items.push(SizeQuantity { size, quantity });
                    }
                }
            }
        }

        let target_date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d").unwrap();

        let filtered_orders: Vec<Order> = parsed_orders
            .into_iter()
            .filter(|order| order.date == target_date)
            .collect();

        return filtered_orders;
    }
    Vec::new()
}

fn get_path_buf(orders: &[Order]) -> Vec<(String, Vec<PathBuf>)> {
    let base_path = Path::new("template");
    let mut path_result = Vec::new();

    for order in orders {
        let folder_name = order.model.replace('\n', "");
        let model_dir = base_path.join(&folder_name);
        let files: Vec<PathBuf> = order
                .items
                .iter()
                .map(|item| model_dir.join(format!("{}.pdf", item.size)))
                .filter(|path| path.exists())
                .collect();
        path_result.push((folder_name, files));
    }

    path_result
}

fn duplicate_pages(input: PathBuf, output: &str, copies: usize) {
    let doc = Document::load(input).unwrap();
    let pages: Vec<_> = doc.get_pages().values().cloned().collect();
    let mut new_doc = Document::with_version("1.5");
    let mut new_pages = Vec::new();

    let mut pages_dict = Dictionary::new();
    pages_dict.set("Type", Object::Name(b"Pages".to_vec()));
    pages_dict.set("Kids", Object::Array(Vec::new()));
    pages_dict.set("Count", Object::Integer(0));
    let pages_id = new_doc.add_object(Object::Dictionary(pages_dict));

    let mut catalog_dict = Dictionary::new();
    catalog_dict.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog_dict.set("Pages", Object::Reference(pages_id));
    let catalog_id = new_doc.add_object(Object::Dictionary(catalog_dict));

    new_doc.trailer.set("Root", Object::Reference(catalog_id));

    for _ in 0..copies {
        for &page_id in &pages {
            let page = doc.get_object(page_id).unwrap().to_owned();
            let new_id = new_doc.add_object(page);
            new_pages.push(new_id);
        }
    }

    let kids_array: Vec<Object> = new_pages.iter().map(|id| Object::Reference(*id)).collect();

    let pages_obj = new_doc.get_object_mut(pages_id).unwrap();
    if let Object::Dictionary(ref mut dict) = *pages_obj {
        dict.set("Kids", Object::Array(kids_array));
        dict.set("Count", Object::Integer((pages.len() * copies) as i64));
    }

    new_doc.save(output).unwrap();
}


#[tokio::main]
async fn main() -> Result<()> {

    dotenv().ok();

    CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider())
        .expect("Failed to install default crypto provider");

    let sa_key_path = env::var("GOOGLE_CREDENTIALS").expect("Json file with Google credentials must be in .env file");
    let spreadsheet_id = env::var("GOOGLE_SHEET_ID").expect("Google Sheets ID must be in .env file");
    let ranges = vec!["Сабира!A8:BW9", "Сабира!A16:BW29"];
    let date_of_order = "2025-09-23";

    let response = get_sheet_values(&sa_key_path, &spreadsheet_id, &ranges).await;

    let orders = parse_order(&date_of_order, response);

    let order_list = get_path_buf(&orders);
    
    for (model, path) in order_list {
        for file in path {
            println!("{:?}", file);
            if file.clone().into_os_string().into_string() == Ok("template/Карандаш35657/48.pdf".to_string()) {
                duplicate_pages(file, "output.pdf", 5);
            }
        }
    }

    Ok(())
}
