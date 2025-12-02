use actix_web::{
    App, HttpResponse, HttpServer, Responder, get, http::header::ContentDisposition, web,
};
use zippity::Builder;

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    println!("Running server on 127.0.0.1:8080");
    HttpServer::new(|| App::new().service(index).service(zip_endpoint))
        .bind(("127.0.0.1", 8080))?
        .run()
        .await
}

#[get("/zip")]
async fn zip_endpoint(
    q: web::Query<std::collections::HashMap<String, String>>,
) -> actix_web::Result<impl Responder> {
    let seed = q
        .get("seed")
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let file_name = format!("zippity_example-{}.zip", seed);
    let directory_name = format!("zippity_example-{}/", seed);
    let mut builder = Builder::<Vec<u8>>::new();
    for i in 0..5 {
        let data = make_svg(seed, i);
        let name = format!("{}image-{}.svg", &directory_name, i);
        builder
            .add_entry(name, data)
            .map_err(actix_web::error::ErrorInternalServerError)?;
    }
    builder
        .add_entry(directory_name, Vec::new())
        .map_err(actix_web::error::ErrorInternalServerError)?
        .directory();
    let responder = builder
        .build()
        .into_responder()
        .customize()
        .append_header(ContentDisposition::attachment(file_name));

    Ok(responder)
}

#[get("/")]
async fn index() -> impl Responder {
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(
            r#"<!DOCTYPE html>
<html><body>
  <form action="/zip" method="get">
    Seed: <input type="text" name="seed" value="42"/>
    <button type="submit">Generate ZIP</button>
  </form>
</body></html>
"#,
        )
}

fn make_svg(seed: u64, i: usize) -> Vec<u8> {
    let mut rng_state =
        ((seed.wrapping_mul(31).wrapping_add(i as u64)).wrapping_mul(0xabcdef)) & 0xffffff;
    let mut random = || {
        rng_state = (rng_state.wrapping_mul(0x9087654).wrapping_add(0x123456)) & 0xffffff;
        rng_state
    };
    // simple random-color rect based on seed + i
    let color = format!("#{:06x}", random());
    let circle_x = random() % 100 + 50;
    let circle_y = random() % 100 + 50;
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="200">
  <rect width="200" height="200" fill="{color}" />
  <circle cx="{circle_x}" cy="{circle_y}" r="80" fill="white" opacity="0.3"/>
</svg>"#,
    );
    svg.into_bytes()
}
