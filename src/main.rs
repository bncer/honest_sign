extern crate google_sheets4 as sheets4;
extern crate hyper;
extern crate hyper_rustls;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{Days, NaiveDate};
use dotenv::dotenv;
use lopdf::dictionary;
use lopdf::{Dictionary, Document, Object, ObjectId};
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
                        let file_path =
                            model_dir.join(format!("{} {}.pdf", articule(&folder_name), size));
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

/// Article number of a model: the digits at the end of its name,
/// e.g. "Карандаш95811" -> "95811".
fn articule(model: &str) -> &str {
    model.trim_start_matches(|c: char| !c.is_ascii_digit())
}

/// Load a label template and write the production date into every page.
fn load_with_date(input: &Path, production_date: &str) -> Document {
    let mut doc = Document::load(input)
        .unwrap_or_else(|e| panic!("Cannot open template {}: {e}", input.display()));

    fix_broken_cmaps(&mut doc);

    let text = "Дата производства:";
    let other_text = format!("Дата производства: {production_date}");

    let page_count = doc.get_pages().len() as u32;
    for page_number in 1..=page_count {
        doc.replace_text(page_number, text, &other_text, None)
            .expect("Failed to replace text");
    }
    doc
}

/// Page attributes that a page can inherit from its parent `Pages` node.
const INHERITABLE_PAGE_KEYS: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

/// Copy of a page dictionary with inherited attributes filled in, so the page
/// still renders the same after being moved into a different page tree.
fn standalone_page(doc: &Document, page_id: ObjectId) -> Dictionary {
    let mut page = doc.get_dictionary(page_id).unwrap().clone();
    let mut parent = page.get(b"Parent").and_then(Object::as_reference).ok();
    while let Some(parent_id) = parent {
        let Ok(node) = doc.get_dictionary(parent_id) else {
            break;
        };
        for key in INHERITABLE_PAGE_KEYS {
            if !page.has(key) {
                if let Ok(value) = node.get(key) {
                    page.set(key, value.clone());
                }
            }
        }
        parent = node.get(b"Parent").and_then(Object::as_reference).ok();
    }
    page
}

/// Build one PDF for an order. For every item (in the given order) the
/// template's pages are repeated until there are `quantity` pages, e.g. 25
/// labels from a 10-page template gives pages 1-10, 1-10, 1-5.
///
/// Repeated pages share the same content streams, fonts and images, so the
/// file stays small no matter how many labels are needed.
fn build_order_pdf(items: &[SizeQuantity], production_date: &str) -> Document {
    let mut out = Document::with_version("1.5");
    let pages_id = out.new_object_id();
    let mut kids: Vec<Object> = Vec::new();

    for item in items {
        let mut doc = load_with_date(&item.file_path, production_date);
        // Move this template's object ids past everything already in `out`.
        doc.renumber_objects_with(out.max_id + 1);

        let source_pages: Vec<Dictionary> = doc
            .get_pages()
            .into_values()
            .map(|id| standalone_page(&doc, id))
            .collect();
        assert!(
            !source_pages.is_empty(),
            "Template {} has no pages",
            item.file_path.display()
        );

        // Take everything except the old catalog and page tree; the new
        // pages below point at the same content, fonts and images.
        for (id, object) in doc.objects {
            out.max_id = out.max_id.max(id.0);
            let is_page_tree = matches!(
                object.type_name(),
                Ok(b"Catalog") | Ok(b"Pages") | Ok(b"Page")
            );
            if !is_page_tree {
                out.objects.insert(id, object);
            }
        }

        for i in 0..item.quantity as usize {
            let mut page = source_pages[i % source_pages.len()].clone();
            page.set("Parent", pages_id);
            kids.push(Object::Reference(out.add_object(page)));
        }
    }

    let count = kids.len() as i64;
    out.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => Object::Array(kids),
            "Count" => count,
        }),
    );
    let catalog_id = out.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    out.trailer.set("Root", catalog_id);

    out.prune_objects();
    out.compress();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_with_date_replaces_production_date() {
        let doc = load_with_date(Path::new("template/Карандаш35663/35663 38.pdf"), "17.11.2026");

        let content = doc.get_page_content(doc.get_pages().values().next().copied().unwrap()).unwrap();
        let text = String::from_utf8(content).unwrap().to_ascii_uppercase();
        // "Дата производства: 17.11.2026" encoded as 2-byte CIDs
        // (CID == Unicode code point for this font)
        assert!(text.contains("04140430044204300020043F0440043E043804370432043E04340441044204320430003A002000310037002E00310031002E0032003000320036"));
    }

    #[test]
    fn build_order_pdf_repeats_pages_up_to_quantity() {
        let item = |size: i64, quantity: i64| SizeQuantity {
            size,
            quantity,
            file_path: PathBuf::from(format!("template/Карандаш35663/35663 {size}.pdf")),
        };
        let mut doc = build_order_pdf(&[item(38, 25), item(40, 10)], "17.11.2026");

        // Round-trip through bytes to make sure the result is a valid PDF.
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        let doc = Document::load_mem(&bytes).unwrap();

        let pages: Vec<ObjectId> = doc.get_pages().into_values().collect();
        assert_eq!(pages.len(), 35);

        let contents = |n: usize| doc.get_dictionary(pages[n]).unwrap().get(b"Contents").unwrap().clone();
        // 25 labels from a 10-page template: 1-10, 1-10, 1-5
        assert_eq!(contents(0), contents(10));
        assert_eq!(contents(0), contents(20));
        assert_eq!(contents(4), contents(24));
        assert_ne!(contents(0), contents(1));
        // The next size starts from its own template
        assert_ne!(contents(25), contents(0));
        assert_eq!(contents(25), contents(34 - 9));
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
    let production_date = "26.11.2026";

    let response = get_sheet_values(&sa_key_path, &spreadsheet_id, &ranges).await;

    let orders = parse_order(&date_of_order, response);

    let output_dir = format!("output/{date_of_order}");
    fs::create_dir_all(&output_dir)?;

    for mut order in orders {
        // Same order as `ls -v res_*.pdf`: smallest size first.
        order.items.sort_by_key(|item| item.size);

        let output_path = format!(
            "{output_dir}/skirt_{}_{}.pdf",
            order.date.format("%d_%m"),
            articule(&order.model)
        );

        println!("{}:", order.model);
        for item in &order.items {
            println!("  {} x {}", item.file_path.display(), item.quantity);
        }

        let mut doc = build_order_pdf(&order.items, production_date);
        doc.save(&output_path)
            .unwrap_or_else(|e| panic!("Cannot save {output_path}: {e}"));

        let total: i64 = order.items.iter().map(|item| item.quantity).sum();
        println!("  -> {output_path} ({total} pages)");
    }

    Ok(())
}
