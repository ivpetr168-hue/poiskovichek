use std::fs;
use std::io::Write;
use std::path::Path;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

fn main() {
    let proba_dir = Path::new("tests").join("proba");
    fs::create_dir_all(&proba_dir).expect("create proba dir");

    // 1. dokument.txt в кодировке cp1251
    let text_cp1251 = "Северный морской путь связывает порты. Корабли уверенно преодолевали арктические льды во время экспедиции.";
    let (encoded, _, _) = encoding_rs::WINDOWS_1251.encode(text_cp1251);
    fs::write(proba_dir.join("dokument.txt"), encoded).expect("write dokument.txt");

    // 2. statya.html в UTF-8
    let html_content = r#"<!DOCTYPE html>
<html lang="ru">
<head><meta charset="utf-8"><title>Статья о садоводстве</title></head>
<body>
  <h1>Весенний уход за плодовыми деревьями</h1>
  <p>Своевременная весенняя обрезка молодых яблонь и груш существенно повышает плодоношение и урожайность сада.</p>
</body>
</html>"#;
    fs::write(proba_dir.join("statya.html"), html_content).expect("write statya.html");

    // 3. kniga.fb2 в UTF-8
    let fb2_content = r#"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0">
  <description>
    <title-info>
      <genre>science</genre>
      <author><first-name>Иван</first-name><last-name>Летописец</last-name></author>
      <book-title>Хроники северных морей</book-title>
    </title-info>
  </description>
  <body>
    <section>
      <title><p>Глава 1</p></title>
      <p>Старинная монастырская летопись повествует о древних походах новгородских купцов через Белое море.</p>
    </section>
  </body>
</FictionBook>"#;
    fs::write(proba_dir.join("kniga.fb2"), fb2_content).expect("write kniga.fb2");

    // 4. zametka.txt в UTF-8
    let zametka_content =
        "Экспозиция геологического музея включает редкие минералы и кристаллические породы.";
    fs::write(proba_dir.join("zametka.txt"), zametka_content).expect("write zametka.txt");

    // 5. otchet.docx (ZIP-контейнер с word/document.xml)
    let docx_path = proba_dir.join("otchet.docx");
    let docx_file = fs::File::create(&docx_path).expect("create docx");
    let mut zip = ZipWriter::new(docx_file);
    let opts = SimpleFileOptions::default();

    zip.start_file("[Content_Types].xml", opts).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"#).unwrap();

    zip.start_file("_rels/.rels", opts).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#).unwrap();

    zip.start_file("word/document.xml", opts).unwrap();
    zip.write_all(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>Годовой финансовый отчет предприятия отражает уверенный рост инвестиций в разработку квантовых алгоритмов.</w:t></w:r></w:p>
  </w:body>
</w:document>"#.as_bytes()).unwrap();
    zip.finish().unwrap();

    // 6. arhiv.zip с вложенным vnutri.txt
    let zip_path = proba_dir.join("arhiv.zip");
    let zip_file = fs::File::create(&zip_path).expect("create zip");
    let mut zip2 = ZipWriter::new(zip_file);
    zip2.start_file("vnutri.txt", opts).unwrap();
    zip2.write_all(
        "Чертеж нового механизма для ветряной мельницы найден в старом архиве.".as_bytes(),
    )
    .unwrap();
    zip2.finish().unwrap();

    // 7. spravka.pdf через lopdf
    let pdf_bytes = make_pdf(Some(
        "Архивная справка подтверждает результаты научных экспериментов в лаборатории.",
    ));
    fs::write(proba_dir.join("spravka.pdf"), pdf_bytes).expect("write spravka.pdf");

    // 8. self-test.txt (запросы: словоформа, фраза, имя файла, файл внутри ZIP)
    let self_test_content = "арктический лед\t1\tdokument.txt\r\nвесенняя обрезка\t1\tstatya.html\r\nspravka\t1\tspravka.pdf\r\nветряной мельницы\t1\tvnutri.txt\r\n";
    fs::write(proba_dir.join("self-test.txt"), self_test_content).expect("write self-test.txt");

    println!("Успешно созданы пробные файлы в tests/proba");
}

fn make_pdf(text: Option<&str>) -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{Document, Object, Stream, dictionary};

    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });

    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! {
            "F1" => font_id,
        },
    });

    let mut operations = Vec::new();
    if let Some(t) = text {
        operations.push(Operation::new("BT", vec![]));
        operations.push(Operation::new("Tf", vec!["F1".into(), 12.into()]));
        operations.push(Operation::new(
            "Tj",
            vec![Object::string_literal(t.as_bytes())],
        ));
        operations.push(Operation::new("ET", vec![]));
    }

    let content = Content { operations };
    let content_stream = Stream::new(dictionary! {}, content.encode().unwrap());
    let content_id = doc.add_object(content_stream);

    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
    });

    let pages = dictionary! {
        "Type" => "Pages",
        "Kids" => vec![page_id.into()],
        "Count" => 1,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
    };
    doc.objects.insert(pages_id, Object::Dictionary(pages));

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut output = Vec::new();
    doc.save_to(&mut output).unwrap();
    output
}
