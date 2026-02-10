extern crate google_sheets4 as sheets4;
extern crate hyper;
extern crate hyper_rustls;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{Days, NaiveDate};
use dotenv::dotenv;
use lopdf::dictionary;
use lopdf::{Document, Object};
use rustls::crypto::CryptoProvider;
use sheets4::{Result, Sheets, api::BatchGetValuesResponse, yup_oauth2};

#[derive(Debug)]
struct Order {
    date: NaiveDate,
    model: String,
    items: Vec<SizeQuantity>,
}

#[derive(Debug)]
struct SizeQuantity {
    size: i64,
    quantity: i64,
    file_path: PathBuf,
}

fn sheets_serial_to_date(serial: f64) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(1899, 12, 30)?.checked_add_days(Days::new(serial.trunc() as u64))
}

async fn get_sheet_values(
    sa_key_path: &str,
    spreadsheet_id: &str,
    ranges: &[&str],
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
        .doit()
        .await
    else {
        panic!("Cannot get data from Google sheets")
    };

    data
}

fn parse_order(date_str: &str, response: BatchGetValuesResponse) -> Vec<Order> {
    if let Some(value_ranges) = response.value_ranges {
        let first_range_data = &value_ranges[0].values.as_ref().unwrap();

        let dates: Vec<NaiveDate> = first_range_data[0]
            .iter()
            .skip(1)
            .filter_map(|v| v.as_f64().and_then(sheets_serial_to_date))
            .collect();

        let models: Vec<String> = first_range_data[1]
            .iter()
            .skip(1)
            .filter_map(|v| v.as_str().map(String::from))
            .map(|model| model.replace('\n', ""))
            .collect();

        let num_orders = dates.len();
        let mut parsed_orders: Vec<Order> = Vec::with_capacity(num_orders);
        for i in 0..num_orders {
            parsed_orders.push(Order {
                date: dates[i],
                model: models[i].clone(),
                items: Vec::new(),
            });
        }

        let second_range_data = &value_ranges[1].values.as_ref().unwrap();

        let base_path = Path::new("template");
        for row in second_range_data.iter() {
            let size = row.get(0).and_then(|v| v.as_i64()).unwrap_or(0);
            if size == 0 {
                continue;
            }

            for col_index in 0..num_orders {
                if let Some(quantity) = row.get(col_index + 1).and_then(|v| v.as_i64()) {
                    let folder_name = parsed_orders[col_index].model.replace('\n', "");
                    let model_dir = base_path.join(&folder_name);
                    if quantity > 0 {
                        let file_path = model_dir.join(format!("{}.pdf", size));
                        parsed_orders[col_index].items.push(SizeQuantity {
                            size,
                            quantity,
                            file_path,
                        });
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

fn duplicate_pages(input: PathBuf, output: &str, copies: i64) {
    let mut doc = Document::load(input).unwrap();

    let text = "Дата производства:".to_string();
    let other_text = "Дата производства: 12.02.2026".to_string();
    let default_str = None;

    let page_count = doc.get_pages().len();

    for page_number in 1..page_count + 1 {
        let _ = doc.replace_text(page_number as u32, &text, &other_text, default_str);
    }

    let pages: Vec<_> = doc.get_pages().values().cloned().collect();

    let mut new_doc = Document::with_version("1.5");

    let catalog_id = new_doc.new_object_id();

    let pages_id = new_doc.new_object_id();

    new_doc.objects.insert(
        catalog_id,
        Object::Dictionary(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id
        }),
    );

    let mut new_pages = Vec::new();

    let mut source_contents = Vec::new();
    for &page_id in &pages {
        if let Ok(page) = doc.get_dictionary(page_id) {
            if let Ok(contents) = page.get(b"Contents") {
                match contents {
                    Object::Reference(id) => match doc.objects.get(id) {
                        None | Some(Object::Stream(_)) => {
                            source_contents.push(*id);
                        }
                        _ => println!("Can not parse content"),
                    },
                    _ => println!("Can not parse content"),
                }
            }
        }
    }

    for _ in 0..copies {
        for &page_id in &pages {
            let page = doc.get_object(page_id).unwrap().to_owned();
            let new_id = new_doc.add_object(page);
            new_pages.push(new_id);
        }
    }

    let mut new_contents = Vec::new();
    for content_id in &source_contents {
        let content = doc.get_object(*content_id).unwrap().to_owned();
        let new_content_id = new_doc.add_object(content);
        new_contents.push(new_content_id);
    }

    let mut font_pages = Vec::new();
    for obj in &doc.objects {
        if let Ok(obj_dict) = doc.get_dictionary(*obj.0) {
            if let Ok(value) = obj_dict.get(b"Type") {
                if *value == Object::from("Font") {
                    let new_id = new_doc.add_object(obj.1.clone());
                    font_pages.push(new_id);
                }
            }
        }
    }

    for i in [0, 2] {
        if let Ok(Object::Dictionary(desc_font)) = new_doc.get_object_mut(font_pages[i]) {
            desc_font.set(
                b"DescendantFonts",
                Object::Array(vec![Object::Reference((font_pages[i + 1].0, 0))]),
            );
        }
    }

    let resources_id = new_doc.add_object(dictionary! {
        "Font" => dictionary! {
            "F0" => font_pages[0],
            "F1" => font_pages[1],
            "F2" => font_pages[2],
            "F3" => font_pages[3],
        },
    });

    for (i, page_id) in new_pages.iter().enumerate() {
        let matching_content = new_contents[i % new_contents.len()];
        if let Ok(Object::Dictionary(dict)) = new_doc.get_object_mut(*page_id) {
            dict.set("Contents", Object::Reference(matching_content));
            dict.set("Resources", resources_id);
        }
    }

    let kids_array: Vec<Object> = new_pages.iter().map(|id| Object::Reference(*id)).collect();

    new_doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(kids_array),
            "Count" => new_pages.len() as u32,
        }),
    );

    new_doc.trailer.set("Root", catalog_id);

    new_doc.save(output).unwrap();
}

fn set_date(input: &str, output: &str) {
    let mut doc = Document::load(input).unwrap();

    doc.version = "1.4".to_string();
    let text = "Дата производства:".to_string();
    let other_text = "Дата производства: 12.02.2026".to_string();
    let default_str = None;

    let page_count = doc.get_pages().len();

    for page_number in 1..page_count + 1 {
        let _ = doc.replace_text(page_number as u32, &text, &other_text, default_str);
    }
    doc.save(&output).unwrap();
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();

    CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider())
        .expect("Failed to install default crypto provider");

    let sa_key_path = env::var("GOOGLE_CREDENTIALS")
        .expect("Json file with Google credentials must be in .env file");
    let spreadsheet_id =
        env::var("GOOGLE_SHEET_ID").expect("Google Sheets ID must be in .env file");
    let ranges = vec!["Sheet1!A8:BW9", "Sheet1!A16:BW30"];
    let date_of_order = "2026-02-06";

    let response = get_sheet_values(&sa_key_path, &spreadsheet_id, &ranges).await;

    let orders = parse_order(&date_of_order, response);

    for order in orders {
        let output_dir = format!("output/{0}/{1}", date_of_order, order.model);
        fs::create_dir_all(&output_dir)?;
        for item in order.items {
            let output_path = format!("{}/output_{}.pdf", &output_dir, item.size);
            println!("{:#?}", item.file_path.to_str().unwrap());
            set_date(item.file_path.to_str().unwrap(), &output_path);
            // duplicate_pages(
            //     item.file_path.to_path_buf(),
            //     &output_path,
            //     item.quantity / 10,
            // );
        }
    }

    Ok(())
}
