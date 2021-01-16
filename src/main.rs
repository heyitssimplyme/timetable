use std::collections::{HashMap, HashSet};
use structopt::StructOpt;

use scraper::{Html, Selector, element_ref::ElementRef};

use serde::Serialize;

use prettytable::{table, row, cell};
use dialoguer::{theme::ColorfulTheme, Select};

static REM_URL: &str = "https://wrem.sis.yorku.ca/Apps/WebObjects/REM.woa/wa/DirectAction/rem"; 
static LOGIN_PAGE: &str = "https://passportyork.yorku.ca/ppylogin/ppylogin";
static LOGOUT_PAGE: &str = "https://passportyork.yorku.ca/ppylogin/ppylogout";

#[derive(Debug, StructOpt)]
#[structopt(name = "timetable", about = "A simple command line program to print out the York timetable")]
struct Cli {
  #[structopt(help = "York Username")]
  username: String,
  #[structopt(help = "York Password")]
  password: String,
  #[structopt(short, long, help = "Output as json rather than a table")]
  json: bool,
}

#[derive(Debug)]
struct SessionsPageData {
  sessions: Vec<String>,
  form_url: String,
  submit_map: HashMap<String, String>,
  session_form_name: String,
}

#[derive(Debug, Serialize)]
struct CourseTime {
  day_time: String,
  duration: String,
}

type CourseData = HashMap<String, HashMap<String, Vec<CourseTime>>>;

async fn auth (client: &reqwest::Client, args: &Cli) -> Result<bool, Box<dyn std::error::Error>> {
  let resp = client.get(REM_URL).send().await?.text().await?;
  
  let mut login_fields: HashMap<String, String> = [
    ("mli".to_owned(), args.username.to_owned()),
    ("password".to_owned(), args.password.to_owned()),
    ("dologin".to_owned(), "Login".to_owned()),
  ].iter().cloned().collect();

  let document = Html::parse_document(&resp);
  let hidden_selector = Selector::parse("input[type='hidden']").unwrap();

  // append all the hiden fields for the auth
  document.select(&hidden_selector).for_each(|element| {
    login_fields.insert(element.value().attr("name").unwrap().to_owned(), element.value().attr("value").unwrap().to_owned());
  });

  let login_resp = client.post(LOGIN_PAGE).form(&login_fields).send().await?;

  let login_resp_content = &login_resp.text().await?;

  // will be authenticated if this string is present in the page
  Ok(login_resp_content.contains("You have successfully authenticated"))
}

async fn logout (client: &reqwest::Client) -> Result<(), Box<dyn std::error::Error>> {
  // a single request is all that is needed
  client.get(LOGOUT_PAGE).send().await?;
  Ok(())
}

fn get_form_fields (doc: &Html) -> Result<(String, HashMap<String, String>), Box<dyn std::error::Error>> {
  let mut form_data: HashMap<String, String> = HashMap::new();

  let form_sel = Selector::parse("form").unwrap();
  let form_url = doc.select(&form_sel).last().unwrap().value().attr("action").unwrap().to_owned();

  let input_sel = Selector::parse("input[type='submit'],input[type='hidden']").unwrap();
  doc.select(&input_sel).for_each(|e| {
    form_data.insert(e.value().attr("name").unwrap().to_owned(), e.value().attr("value").unwrap().to_owned());
  });

  Ok((form_url, form_data))
}

async fn parse_sessions_page (client: &reqwest::Client) -> Result<SessionsPageData, Box<dyn std::error::Error>> {
  let resp = client.get(REM_URL).send().await?.text().await?;

  let doc = Html::parse_document(&resp);

  let select_sel = Selector::parse("select").unwrap();
  let options_sel = Selector::parse("option").unwrap();

  let select = doc.select(&select_sel).next().unwrap();
  let select_name = select.value().attr("name").unwrap().to_owned();

  let session_ids = select.select(&options_sel).map(|s| s.inner_html()).collect::<Vec<_>>();

  let (form_url, form_data) = get_form_fields(&doc)?;

  Ok(SessionsPageData {
    form_url,
    submit_map: form_data,
    sessions: session_ids,
    session_form_name: select_name,
  })
}

async fn get_course_details (client: &reqwest::Client, id: i32, sessions_data: &mut SessionsPageData) -> Result<Html, Box<dyn std::error::Error>> {
  sessions_data.submit_map.insert(sessions_data.session_form_name.clone(), id.to_string());
  let session_summary = client.post(&sessions_data.form_url).form(&sessions_data.submit_map).send().await?.text().await?;

  let doc = Html::parse_document(&session_summary);

  let (form_url, form_data) = get_form_fields(&doc)?;

  let course_details = client.post(&form_url).form(&form_data).send().await?.text().await?;

  let doc = Html::parse_document(&course_details);

  Ok(doc)
}

fn select_cells(element: ElementRef, selector: &Selector) -> Vec<String> {
  element.select(selector).map(|el| el.text().map(|t| t.trim()).collect()).collect()
}

async fn parse_timetable (page: &Html) -> Result<CourseData, Box<dyn std::error::Error>> {
  let table_sel = Selector::parse("body > form > div:nth-child(1) > table > tbody > tr:nth-child(4) > td:nth-child(2) > table > tbody > tr > td > table:nth-child(8)").unwrap();
  
  let table = page.select(&table_sel).next().unwrap();

  let tr_sel = Selector::parse("tr").unwrap();
  let td_sel = Selector::parse("td").unwrap();

  let rows = table.select(&tr_sel);
  let data: Vec<Vec<String>> = rows.map(|tr| select_cells(tr, &td_sel)).collect();

  let mut courses: CourseData = HashMap::new();

  let mut course = "";
  let mut format = "";

  for row in data.iter().skip(1) {
    if row.len() < 4 { continue; }
    if row[2].is_empty() || row[3].is_empty() { continue; }

    if !row[0].is_empty() {
      course = &row[0];
    }
    if !row[1].is_empty() {
      format = &row[1];
    }

    let day_time = row[2].to_owned();
    let duration = row[3].to_owned();

    if !courses.contains_key(course) {
      courses.insert(course.to_owned(), HashMap::new());
    }

    let course_data = courses.get_mut(course).unwrap();
    if !course_data.contains_key(format) {
      course_data.insert(format.to_owned(), Vec::new());
    }

    let times = course_data.get_mut(format).unwrap();
    times.push(CourseTime { day_time, duration });
  }

  Ok(courses)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
  let args = Cli::from_args();

  let client = reqwest::Client::builder()
    .cookie_store(true)
    .build()?;

  let authenticated = auth(&client, &args).await?;
  if !authenticated {
    panic!("Could not authenticate!");
  }

  let mut sessions_data = parse_sessions_page(&client).await?;
  
  let selection = Select::with_theme(&ColorfulTheme::default())
    .with_prompt("Choose which term")
    .default(0)
    .items(&sessions_data.sessions[1..])
    .interact()
    .unwrap() as i32;

  // get to the session summary page
  let details_doc = get_course_details(&client, selection, &mut sessions_data).await?;
  let course_data = parse_timetable(&details_doc).await?;

  // get the season from the string
  let seasons: HashSet<String> = course_data.keys().map(|k| k.split_ascii_whitespace().next().unwrap().to_owned()).collect();
  let mut seasons: Vec<String> = seasons.iter().cloned().collect();
  seasons.insert(0, "All".into());

  let season_id = Select::with_theme(&ColorfulTheme::default())
    .with_prompt("Choose which season")
    .default(0)
    .items(&seasons)
    .interact()
    .unwrap();

  let season = &seasons[season_id];
  println!("{:?}", season);

  if args.json {
    println!("{}", serde_json::to_string(&course_data)?);
  } else {
    let mut table = table![["Course", "Lecture Times"]];

    for (course, lecture_data) in course_data {
      // only for that season unless all
      if !course.starts_with(season) && season != "All" { continue; }

      let mut inner_table = table![];
      for (format, lecture_times) in lecture_data {
        let mut times_table = table![["Time", "Duration"]];
        for lecture_time in lecture_times {
          times_table.add_row(row![lecture_time.day_time, lecture_time.duration]);
        }
        inner_table.add_row(row![format, l->times_table]);
      }
      table.add_row(row![course, l->inner_table]);
    }

    table.printstd();
  }

  logout(&client).await?;

  Ok(())
}
