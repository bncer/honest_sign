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
                    let articule = folder_name.strip_prefix("Карандаш").unwrap_or(&folder_name);
                    if quantity > 0 {
                        let file_path = model_dir.join(format!("{articule} {}.pdf", size));
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

/// The label templates have broken ToUnicode CMaps, which prevent
/// `Document::replace_text` from working at all:
///
/// 1. The main label fonts (Identity-H) ship a CMap `<0000> <FFFF> <0000>`
///    that maps every CID to U+0000, so lopdf decodes all text as NUL chars
///    and never finds the search string. Rewrite such CMaps with a correct
///    identity mapping (CID == Unicode code point for every glyph used in
///    these labels: ASCII 0x20-0x7E and Cyrillic 0x410-0x45F).
///
/// 2. The CaptionBold font CMap contains 4-byte (odd-length hex) target
///    strings like `<10300>` (UTF-16 surrogate pairs) which lopdf 0.38's
///    CMap parser rejects, so `replace_text` fails with
///    `ToUnicodeCMap(Parse(Error))` before touching any text. Drop the
///    unparseable mapping lines; the CIDs actually used by that font are
///    covered by the remaining ranges. ToUnicode only affects text
///    extraction, not rendering.
fn fix_broken_cmaps(doc: &mut Document) {
    const CORRECT_CMAP: &[u8] = b"/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo
<</Registry (Adobe)
/Ordering (UCS)
/Supplement 0
>> def
/CMapName /Adobe-Identity-UCS def
/CMapType 2 def
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
2 beginbfrange
<0020> <007E> <0020>
<0410> <045F> <0410>
endbfrange
endcmap
CMapName currentdict /CMap defineresource pop
end
end";

    for (_, obj) in doc.objects.iter_mut() {
        if let Object::Stream(stream) = obj {
            let Ok(content) = stream.get_plain_content() else {
                continue;
            };
            if !content.windows(9).any(|w| w == b"begincmap") {
                continue;
            }

            if content.windows(20).any(|w| w == b"<0000> <FFFF> <0000>") {
                stream.set_plain_content(CORRECT_CMAP.to_vec());
                continue;
            }

            let text = String::from_utf8_lossy(&content).to_string();
            let lines: Vec<&str> = text.lines().collect();
            // Only mapping lines (bfrange/bfchar entries) start with '<';
            // skip dictionary lines like `/CIDSystemInfo <<...>> def`.
            let kept: Vec<&str> = lines
                .iter()
                .copied()
                .filter(|line| !line.trim_start().starts_with('<') || !line_has_odd_hex_string(line))
                .collect();
            if kept.len() != lines.len() {
                stream.set_plain_content(kept.join("\n").into_bytes());
            }
        }
    }
}

/// Returns true if the line contains a `<...>` hex string with an odd number
/// of hex digits, which lopdf's CMap parser cannot handle.
fn line_has_odd_hex_string(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if let Some(end) = bytes[i..].iter().position(|&b| b == b'>') {
                // hex digits are the bytes between '<' and '>'
                if (end - 1) % 2 != 0 {
                    return true;
                }
                i += end + 1;
                continue;
            }
        }
        i += 1;
    }
    false
}

fn duplicate_pages(input: PathBuf, output: &str, copies: i64) {
    let mut doc = Document::load(input).unwrap();

    fix_broken_cmaps(&mut doc);

    let text = "Дата производства:".to_string();
    let other_text = "Дата производства: 27.05.2026".to_string();
    let default_str = None;

    let page_count = doc.get_pages().len();

    for page_number in 1..page_count + 1 {
        doc.replace_text(page_number as u32, &text, &other_text, default_str)
            .expect("Failed to replace text");
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

    fix_broken_cmaps(&mut doc);

    doc.version = "1.4".to_string();
    let text = "Дата производства:".to_string();
    let other_text = "Дата производства: 26.11.2026".to_string();
    let default_str = None;

    let page_count = doc.get_pages().len();

    for page_number in 1..page_count + 1 {
        doc.replace_text(page_number as u32, &text, &other_text, default_str)
            .expect("Failed to replace text");
    }
    doc.save(&output).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_date_replaces_production_date() {
        let out = "/tmp/honest_sign_test_output.pdf";
        set_date("template/Карандаш35663/35663 38.pdf", out);

        let doc = Document::load(out).unwrap();
        let content = doc.get_page_content(doc.get_pages().values().next().copied().unwrap()).unwrap();
        let text = String::from_utf8(content).unwrap().to_ascii_uppercase();
        // "Дата производства: 17.11.2026" encoded as 2-byte CIDs
        // (CID == Unicode code point for this font)
        assert!(text.contains("04140430044204300020043F0440043E043804370432043E04340441044204320430003A002000310037002E00310031002E0032003000320036"));
    }
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
    // Ranges must start at column A: column A holds the row labels / sizes,
    // and each following column is one order.
    let ranges = vec!["Sheet1!A8:DX9", "Sheet1!A17:DX31"];
    let date_of_order = "2026-11-18";

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
