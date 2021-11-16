use crate::tori::parse::*;
use crate::Database;
use crate::ItemHistory;
use crate::Mutex;
use chrono::{Local, TimeZone};
use serenity::{client::Context, http::Http};
use std::sync::Arc;
use serde_json::Value;

#[derive(Clone)]
pub struct Vahti {
    pub url: String,
    pub user_id: i64,
    pub last_updated: i64,
}

pub async fn new_vahti(ctx: &Context, url: &str, userid: u64) -> String {
    let db = ctx.data.read().await.get::<Database>().unwrap().clone();
    if db
        .fetch_vahti(url, userid.try_into().unwrap())
        .await
        .is_ok()
    {
        info!("Not adding a pre-defined Vahti {} for user {}", url, userid);
        return "Vahti on jo määritelty!".to_string();
    }
    match db.add_vahti_entry(url, userid.try_into().unwrap()).await {
        Ok(_) => "Vahti lisätty!".to_string(),
        Err(_) => "Virhe tapahtui vahdin lisäyksessä!".to_string(),
    }
}

pub async fn remove_vahti(ctx: &Context, url: &str, userid: u64) -> String {
    let db = ctx.data.read().await.get::<Database>().unwrap().clone();
    if db
        .fetch_vahti(url, userid.try_into().unwrap())
        .await.is_err()
    {
        info!("Not removing a nonexistant vahti!");
        return "Kyseistä vahtia ei ole määritelyt, tarkista että kirjoiti linkin oikein".to_string();
    }
    match db.remove_vahti_entry(url, userid.try_into().unwrap()).await {
        Ok(_) => "Vahti poistettu!".to_string(),
        Err(_) => "Virhe tapahtui vahdin poistamisessa!".to_string(),
    }
}

fn vahti_to_api(vahti: &str) -> String {
    let mut url = "https://api.tori.fi/api/v1.2/public/ads".to_owned()
        + &vahti.to_owned()[vahti.find('?').unwrap()..];
    let mut startprice: String = "".to_string();
    let mut endprice: String = "".to_string();
    let mut price_set = false;
    if url.contains("ps=") {
        let index = url.find("ps=").unwrap();
        let endindex = url[index..].find('&').unwrap_or(url.len()-index);
        startprice = url[index+3..endindex+index].to_string();
        price_set = true;
   }
    if url.contains("pe=") {
        let index = url.find("pe=").unwrap();
        let endindex = url[index..].find('&').unwrap_or(url.len()-index);
        endprice = url[index+3..endindex+index].to_string();
        price_set = true;
    }
    url = url.replace("cg=", "category=");
    // because in the API category=0 yealds no results and in the search it just means
    // no category was specified
    url = url.replace("&category=0", "");
    if url.contains("w=") {
        let region;
        let index = url.find("w=").unwrap();
        let endindex = url[index..].find('&').unwrap_or(url.len()-index);
        let num = url[index+2..endindex+index].parse::<i32>().unwrap();
        if num >= 100 {
            region = num-100;
            url = url.replace(&url[index..endindex+index], &format!("region={}", region));
        } else if url.contains("ca=") {
            let nindex = url.find("ca=").unwrap();
            let nendindex = url[nindex..].find('&').unwrap_or(url.len()-nindex);
            let num = url[nindex+3..nendindex+nindex].parse::<i32>().unwrap();
            region = num;
            url = url.replace(&url[index..endindex+index], &format!("region={}", region));
        }
    } else {
        url = url.replace("ca=", "region=");
    }
    url = url.replace("st=", "ad_type=");
    url = url.replace("m=", "area=");
    url = url.replace("_s", ""); // FIXME: not a good solution
    if price_set {
        url = url + &format!("&suborder={}-{}", &startprice, &endprice);
    }
    url
}

pub async fn is_valid_url(url: &str) -> bool {
    if !url.starts_with("https://www.tori.fi") {
        return false;
    }
    if !url.contains('?') {
        return false;
    }
    let url = vahti_to_api(url);
    let response = reqwest::get(&url).await.unwrap().text().await.unwrap();
    let response_json: Value = serde_json::from_str(&response).unwrap();
    if let Some(counter_map) = response_json["counter_map"].as_object() {
        if let Some(amount) = counter_map["all"].as_i64() {
            amount > 0
        } else {
            false
        }
    } else {
       false
    }
}

pub async fn update_all_vahtis(
    db: Arc<Database>,
    itemhistory: &mut Arc<Mutex<ItemHistory>>,
    http: &Http,
) {
    itemhistory.lock().await.purge_old();
    let vahtis = db.fetch_all_vahtis().await.unwrap();
    update_vahtis(db, itemhistory, http, vahtis).await;
}

pub async fn update_vahtis(
    db: Arc<Database>,
    itemhistory: &mut Arc<Mutex<ItemHistory>>,
    http: &Http,
    vahtis: Vec<Vahti>,
) {
    let mut currenturl = String::new();
    let mut currentitems = Vec::new();
    let test = std::time::Instant::now();
    for vahtichunks in vahtis.chunks(5) {
        let vahtichunks = vahtichunks.iter().zip(vahtichunks.iter().map(|vahti| {
            if currenturl != vahti.url {
                currenturl = vahti.url.clone();
                let url = vahti_to_api(&currenturl);
                info!("Sending query: {}", url);
                let response = reqwest::get(url);
                Some(response)
            } else {
                None
            }
        }));
        for (vahti, request) in vahtichunks {
            if let Some(req) = request {
                currentitems = api_parse_after(
                    &req.await.unwrap().text().await.unwrap(),
                    vahti.last_updated,
                )
                .await;
            }
            if !currentitems.is_empty() {
                db.vahti_updated(vahti.clone(), Some(currentitems[0].published))
                    .await
                    .unwrap();
            }
            info!("Got {} items", currentitems.len());
            for item in currentitems.iter().rev() {
                if itemhistory.lock().await.contains(item.ad_id, vahti.user_id) {
                    info!("Item {} in itemhistory! Skipping!", item.ad_id);
                    continue;
                }
                itemhistory
                    .lock()
                    .await
                    .add_item(item.ad_id,vahti.user_id, chrono::Local::now().timestamp());
                let user = http
                    .get_user(vahti.user_id.try_into().unwrap())
                    .await
                    .unwrap();
                user.dm(http, |m| {
                    m.embed(|e| {
                        e.color(serenity::utils::Color::DARK_GREEN);
                        e.description(format!("[{}]({})\n[Hakulinkki]({})", item.title, item.url, vahti.url));
                        e.field("Hinta", format!("{} €", item.price), true);
                        e.field("Myyjä", item.seller_name.clone(), true);
                        e.field("Sijainti", item.location.clone(), true);
                        e.field(
                            "Ilmoitus Jätetty",
                            Local.timestamp(item.published, 0).format("%d/%m/%Y %R"),
                            true,
                        );
                        e.field("Ilmoitustyyppi", item.ad_type.to_string(), true);
                        e.image(item.img_url.clone())
                    })
                })
                .await
                .unwrap();
            }
        }
    }
    info!("Finished requests in {} ms", test.elapsed().as_millis());
}
