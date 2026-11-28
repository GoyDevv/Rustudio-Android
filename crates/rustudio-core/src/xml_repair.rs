const NAMES: [&str; 12] = [
    "X", "Y", "Z", "R00", "R01", "R02", "R10", "R11", "R12", "R20", "R21", "R22",
];
const IDENTITY: [&str; 12] = [
    "0", "0", "0", "1", "0", "0", "0", "1", "0", "0", "0", "1",
];

pub fn repair_cframes(xml: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(xml).ok()?;
    let mut out = String::with_capacity(text.len() + 1024);
    let mut rest = text;
    let mut changed = false;
    while let Some(start) = next_frame_start(rest) {
        out.push_str(&rest[..start.0]);
        let tag = start.1;
        let after = &rest[start.0..];
        let Some(open_end) = after.find('>') else {
            out.push_str(after);
            rest = "";
            break;
        };
        let open = &after[..=open_end];
        if open.ends_with("/>") {
            let (fixed, did) = canonical(open, "", tag);
            changed |= did;
            out.push_str(&fixed);
            rest = &after[open_end + 1..];
            continue;
        }
        let close = format!("</{tag}>");
        let Some(close_at) = after[open_end + 1..].find(&close) else {
            out.push_str(after);
            rest = "";
            break;
        };
        let body = &after[open_end + 1..open_end + 1 + close_at];
        let consumed = open_end + 1 + close_at + close.len();
        if body.contains('<') && body.contains("<orientId") {
            out.push_str(&after[..consumed]);
        } else {
            let (fixed, did) = canonical(open, body, tag);
            changed |= did;
            out.push_str(&fixed);
        }
        rest = &after[consumed..];
    }
    out.push_str(rest);
    changed.then(|| out.into_bytes())
}

fn next_frame_start(s: &str) -> Option<(usize, &'static str)> {
    let a = find_tag(s, "CoordinateFrame");
    let b = find_tag(s, "CFrame");
    match (a, b) {
        (Some(x), Some(y)) if y < x => Some((y, "CFrame")),
        (Some(x), _) => Some((x, "CoordinateFrame")),
        (None, Some(y)) => Some((y, "CFrame")),
        (None, None) => None,
    }
}

fn find_tag(s: &str, tag: &str) -> Option<usize> {
    let needle = format!("<{tag}");
    let mut from = 0;
    while let Some(i) = s[from..].find(&needle) {
        let at = from + i;
        let next = s.as_bytes().get(at + needle.len()).copied();
        if matches!(next, Some(b'>') | Some(b' ') | Some(b'/') | Some(b'\n') | Some(b'\t') | Some(b'\r')) {
            return Some(at);
        }
        from = at + needle.len();
    }
    None
}

fn canonical(open: &str, body: &str, tag: &str) -> (String, bool) {
    let mut vals: [Option<String>; 12] = Default::default();
    let mut rest = body;
    while let Some(lt) = rest.find('<') {
        let tail = &rest[lt + 1..];
        let Some(gt) = tail.find('>') else { break };
        let head = &tail[..gt];
        if head.starts_with('/') || head.starts_with('!') || head.starts_with('?') {
            rest = &tail[gt + 1..];
            continue;
        }
        let self_closed = head.ends_with('/');
        let name = head.trim_end_matches('/').split_whitespace().next().unwrap_or("");
        let (value, adv) = if self_closed {
            (String::new(), gt + 1)
        } else {
            let close = format!("</{name}>");
            match tail[gt + 1..].find(&close) {
                Some(c) => (tail[gt + 1..gt + 1 + c].trim().to_string(), gt + 1 + c + close.len()),
                None => (String::new(), gt + 1),
            }
        };
        if let Some(i) = NAMES.iter().position(|n| *n == name) {
            if vals[i].is_none() {
                vals[i] = Some(value);
            }
        }
        rest = &tail[adv..];
    }
    let open_clean = open.trim_end_matches("/>").trim_end_matches('>');
    let mut s = String::new();
    s.push_str(open_clean);
    s.push('>');
    for (i, n) in NAMES.iter().enumerate() {
        let v = match &vals[i] {
            Some(v) if v.parse::<f32>().is_ok() || matches!(v.as_str(), "INF" | "-INF" | "NAN") => v.clone(),
            _ => IDENTITY[i].to_string(),
        };
        s.push_str(&format!("<{n}>{v}</{n}>"));
    }
    s.push_str(&format!("</{tag}>"));
    let original: String = format!("{open}{body}</{tag}>");
    let normalize = |t: &str| t.split_whitespace().collect::<String>();
    let same = open.ends_with("/>") == false && normalize(&original) == normalize(&s);
    (s, !same)
}

pub fn explain(xml: &[u8], err: &str) -> String {
    let line = err
        .split("line ")
        .nth(1)
        .and_then(|t| t.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse::<usize>().ok());
    let Some(line) = line else { return err.to_string() };
    let text = String::from_utf8_lossy(xml);
    let lo = line.saturating_sub(8).max(1);
    let mut out = format!("{err}\n[xml-repair could not fix this] lines {lo}-{}:", line + 3);
    for (i, l) in text.lines().enumerate() {
        let n = i + 1;
        if n >= lo && n <= line + 3 {
            let cut: String = l.trim().chars().take(110).collect();
            out.push_str(&format!("\n{n}: {cut}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_frame_untouched() {
        let x = "<roblox><CoordinateFrame name=\"CFrame\"><X>1</X><Y>2</Y><Z>3</Z><R00>1</R00><R01>0</R01><R02>0</R02><R10>0</R10><R11>1</R11><R12>0</R12><R20>0</R20><R21>0</R21><R22>1</R22></CoordinateFrame></roblox>";
        assert!(repair_cframes(x.as_bytes()).is_none());
    }

    #[test]
    fn missing_and_reordered_tags_are_filled() {
        let x = "<roblox><CoordinateFrame name=\"CFrame\"><Z>3</Z><X>1</X><R11>0.5</R11></CoordinateFrame></roblox>";
        let out = String::from_utf8(repair_cframes(x.as_bytes()).unwrap()).unwrap();
        assert!(out.contains("<X>1</X><Y>0</Y><Z>3</Z><R00>1</R00><R01>0</R01><R02>0</R02><R10>0</R10><R11>0.5</R11>"));
    }

    #[test]
    fn self_closing_tags_default() {
        let x = "<CoordinateFrame name=\"c\"><X/><Y>4</Y></CoordinateFrame>";
        let out = String::from_utf8(repair_cframes(x.as_bytes()).unwrap()).unwrap();
        assert!(out.contains("<X>0</X><Y>4</Y>"));
    }

    #[test]
    fn optional_cframe_inner_is_repaired() {
        let x = "<OptionalCoordinateFrame name=\"P\"><CFrame><X>1</X><R11>1</R11></CFrame></OptionalCoordinateFrame>";
        let out = String::from_utf8(repair_cframes(x.as_bytes()).unwrap()).unwrap();
        assert!(out.contains("<OptionalCoordinateFrame name=\"P\"><CFrame><X>1</X><Y>0</Y>"));
        assert!(out.ends_with("</CFrame></OptionalCoordinateFrame>"));
    }
}

#[cfg(test)]
mod decode_tests {
    use crate::doc::Document;

    const PLACE: &str = r#"<roblox xmlns:xmime="http://www.w3.org/2005/05/xmlmime" version="4"><Item class="Workspace" referent="RBX0"><Properties><string name="Name">Workspace</string></Properties><Item class="Part" referent="RBX1"><Properties><string name="Name">P</string><CoordinateFrame name="CFrame"><X>1</X><Y>2</Y><Z>3</Z><R00>1</R00><R01>0</R01><R02>0</R02><R11>1</R11><R12>0</R12><R20>0</R20><R21>0</R21><R22>1</R22></CoordinateFrame></Properties></Item></Item></roblox>"#;

    #[test]
    fn place_with_a_missing_cframe_tag_opens() {
        let dom = Document::decode(PLACE.as_bytes()).expect("opens");
        let part = dom
            .descendants()
            .find(|i| i.class == "Part")
            .expect("part kept");
        assert_eq!(part.name, "P");
    }
}

#[cfg(test)]
mod explain_tests {
    use super::explain;

    #[test]
    fn shows_lines_around_the_error() {
        let xml = "<a>\n<b/>\n<c/>\n<d/>";
        let out = explain(xml.as_bytes(), "line 3, column 0: boom");
        assert!(out.contains("3: <c/>") && out.contains("1: <a>"));
    }

    #[test]
    fn keeps_the_error_without_a_line() {
        assert_eq!(explain(b"x", "no position"), "no position");
    }
}
