use std::collections::HashMap;

use quick_xml::events::Event;
use quick_xml::Reader;
use thiserror::Error;

pub const DAV: &str = "DAV:";
pub const CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
pub const CARDDAV: &str = "urn:ietf:params:xml:ns:carddav";
pub const CALENDARSERVER: &str = "http://calendarserver.org/ns/";
pub const APPLE: &str = "http://apple.com/ns/ical/";
pub const INFCLOUD: &str = "http://inf-it.com/ns/caldav/";
pub const OXICALE: &str = "https://oxicale.invalid/ns/";

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum XmlError {
    #[error("malformed XML")]
    Malformed,
    #[error("XML documents with DTDs or entities are not allowed")]
    DtdRejected,
    #[error("XML document is too large")]
    TooLarge,
    #[error("XML document is too deep")]
    TooDeep,
    #[error("XML document has too many elements")]
    TooComplex,
    #[error("XML document has no root element")]
    MissingRoot,
    #[error("XML document has multiple root elements")]
    MultipleRoots,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Element {
    pub local: String,
    pub prefix: Option<String>,
    pub namespace: String,
    pub attributes: Vec<(String, String)>,
    pub children: Vec<Element>,
    pub text: String,
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| {
                key == name
                    || key
                        .split_once(':')
                        .map(|(_, local)| local == name)
                        .unwrap_or(false)
            })
            .map(|(_, value)| value.as_str())
    }

    pub fn child(&self, namespace: &str, local: &str) -> Option<&Element> {
        self.children
            .iter()
            .find(|child| child.local == local && child.namespace == namespace)
    }

    pub fn children_named<'a>(
        &'a self,
        namespace: &'a str,
        local: &'a str,
    ) -> impl Iterator<Item = &'a Element> + 'a {
        self.children
            .iter()
            .filter(move |child| child.local == local && child.namespace == namespace)
    }

    pub fn find(&self, local: &str) -> Option<&Element> {
        self.children.iter().find(|child| child.local == local)
    }

    pub fn text_trimmed(&self) -> &str {
        self.text.trim()
    }
}

const MAX_ELEMENTS: usize = 50_000;

pub fn parse_xml(input: &[u8], max_bytes: usize, max_depth: usize) -> Result<Element, XmlError> {
    if input.len() > max_bytes {
        return Err(XmlError::TooLarge);
    }
    let lower = String::from_utf8_lossy(input).to_ascii_lowercase();
    if lower.contains("<!doctype") || lower.contains("<!entity") {
        return Err(XmlError::DtdRejected);
    }
    let mut reader = Reader::from_reader(input);
    reader.config_mut().trim_text(false);
    reader.config_mut().check_end_names = true;

    struct Frame {
        element: Element,
        namespaces: HashMap<String, String>,
    }

    let mut stack: Vec<Frame> = Vec::new();
    let mut root: Option<Element> = None;
    let mut element_count = 0usize;
    let mut buffer = Vec::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(start)) => {
                element_count += 1;
                if element_count > MAX_ELEMENTS {
                    return Err(XmlError::TooComplex);
                }
                if stack.len() >= max_depth {
                    return Err(XmlError::TooDeep);
                }
                let mut namespaces = stack
                    .last()
                    .map(|frame| frame.namespaces.clone())
                    .unwrap_or_default();
                let mut attributes = Vec::new();
                for attr in start.attributes().with_checks(true) {
                    let attr = attr.map_err(|_| XmlError::Malformed)?;
                    let key = String::from_utf8(attr.key.as_ref().to_vec())
                        .map_err(|_| XmlError::Malformed)?;
                    let value = attr
                        .unescape_value()
                        .map_err(|_| XmlError::Malformed)?
                        .into_owned();
                    if key == "xmlns" {
                        namespaces.insert(String::new(), value);
                    } else if let Some(prefix) = key.strip_prefix("xmlns:") {
                        namespaces.insert(prefix.to_string(), value);
                    } else {
                        attributes.push((key, value));
                    }
                }
                let raw = String::from_utf8(start.name().as_ref().to_vec())
                    .map_err(|_| XmlError::Malformed)?;
                let (prefix, local) = split_qname(&raw)?;
                let namespace = namespaces.get(&prefix).cloned().unwrap_or_default();
                stack.push(Frame {
                    element: Element {
                        local,
                        prefix: if prefix.is_empty() {
                            None
                        } else {
                            Some(prefix)
                        },
                        namespace,
                        attributes,
                        children: Vec::new(),
                        text: String::new(),
                    },
                    namespaces,
                });
            }
            Ok(Event::Empty(start)) => {
                element_count += 1;
                if element_count > MAX_ELEMENTS {
                    return Err(XmlError::TooComplex);
                }
                if stack.len() >= max_depth {
                    return Err(XmlError::TooDeep);
                }
                let mut namespaces = stack
                    .last()
                    .map(|frame| frame.namespaces.clone())
                    .unwrap_or_default();
                let mut attributes = Vec::new();
                for attr in start.attributes().with_checks(true) {
                    let attr = attr.map_err(|_| XmlError::Malformed)?;
                    let key = String::from_utf8(attr.key.as_ref().to_vec())
                        .map_err(|_| XmlError::Malformed)?;
                    let value = attr
                        .unescape_value()
                        .map_err(|_| XmlError::Malformed)?
                        .into_owned();
                    if key == "xmlns" {
                        namespaces.insert(String::new(), value);
                    } else if let Some(prefix) = key.strip_prefix("xmlns:") {
                        namespaces.insert(prefix.to_string(), value);
                    } else {
                        attributes.push((key, value));
                    }
                }
                let raw = String::from_utf8(start.name().as_ref().to_vec())
                    .map_err(|_| XmlError::Malformed)?;
                let (prefix, local) = split_qname(&raw)?;
                let namespace = namespaces.get(&prefix).cloned().unwrap_or_default();
                let element = Element {
                    local,
                    prefix: if prefix.is_empty() {
                        None
                    } else {
                        Some(prefix)
                    },
                    namespace,
                    attributes,
                    children: Vec::new(),
                    text: String::new(),
                };
                if let Some(parent) = stack.last_mut() {
                    parent.element.children.push(element);
                } else if root.is_none() {
                    root = Some(element);
                } else {
                    return Err(XmlError::MultipleRoots);
                }
            }
            Ok(Event::End(_)) => {
                let frame = stack.pop().ok_or(XmlError::Malformed)?;
                if let Some(parent) = stack.last_mut() {
                    parent.element.children.push(frame.element);
                } else if root.is_none() {
                    root = Some(frame.element);
                } else {
                    return Err(XmlError::MultipleRoots);
                }
            }
            Ok(Event::Text(text)) => {
                let decoded = text.unescape().map_err(|_| XmlError::Malformed)?;
                if let Some(frame) = stack.last_mut() {
                    frame.element.text.push_str(&decoded);
                } else if !decoded.trim().is_empty() {
                    return Err(XmlError::Malformed);
                }
            }
            Ok(Event::CData(text)) => {
                if let Some(frame) = stack.last_mut() {
                    frame
                        .element
                        .text
                        .push_str(&String::from_utf8_lossy(text.as_ref()));
                }
            }
            Ok(Event::DocType(_)) => return Err(XmlError::DtdRejected),
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => return Err(XmlError::Malformed),
        }
        buffer.clear();
    }
    if !stack.is_empty() {
        return Err(XmlError::Malformed);
    }
    root.ok_or(XmlError::MissingRoot)
}

fn split_qname(raw: &str) -> Result<(String, String), XmlError> {
    if let Some((prefix, local)) = raw.split_once(':') {
        if prefix.is_empty() || local.is_empty() || local.contains(':') {
            return Err(XmlError::Malformed);
        }
        Ok((prefix.to_string(), local.to_string()))
    } else if raw.is_empty() {
        Err(XmlError::Malformed)
    } else {
        Ok((String::new(), raw.to_string()))
    }
}

pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

pub fn qname(prefix: &str, local: &str) -> String {
    if prefix.is_empty() {
        local.to_string()
    } else {
        format!("{prefix}:{local}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_namespaced_document() {
        let xml = br#"<?xml version="1.0"?>
<D:propfind xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop><C:calendar-home-set/><D:displayname/></D:prop>
</D:propfind>"#;
        let root = parse_xml(xml, 1024 * 1024, 32).unwrap();
        assert_eq!(root.local, "propfind");
        assert_eq!(root.namespace, DAV);
        let prop = root.child(DAV, "prop").unwrap();
        assert_eq!(prop.children.len(), 2);
        assert_eq!(prop.children[0].namespace, CALDAV);
        assert_eq!(prop.children[1].namespace, DAV);
    }

    #[test]
    fn inherits_namespaces() {
        let xml = br#"<D:a xmlns:D="DAV:"><D:b><D:c/></D:b></D:a>"#;
        let root = parse_xml(xml, 1000, 32).unwrap();
        let b = root.child(DAV, "b").unwrap();
        assert!(b.child(DAV, "c").is_some());
    }

    #[test]
    fn reads_text_and_attributes() {
        let xml = br#"<D:sync-collection xmlns:D="DAV:"><D:sync-token>abc</D:sync-token><D:x n="1"/></D:sync-collection>"#;
        let root = parse_xml(xml, 1000, 32).unwrap();
        assert_eq!(root.child(DAV, "sync-token").unwrap().text_trimmed(), "abc");
        assert_eq!(root.child(DAV, "x").unwrap().attr("n"), Some("1"));
    }

    #[test]
    fn rejects_dtd_and_entities() {
        for xml in [
            b"<!DOCTYPE foo [<!ENTITY bar \"baz\">]><D:a xmlns:D=\"DAV:\"/>".as_slice(),
            b"<!ENTITY xxe SYSTEM \"file:///etc/passwd\"><D:a xmlns:D=\"DAV:\"/>".as_slice(),
            b"<D:a xmlns:D=\"DAV:\"><D:b></D:a>".as_slice(),
            b"".as_slice(),
            b"<a/><b/>".as_slice(),
        ] {
            assert!(parse_xml(xml, 1024, 32).is_err());
        }
    }

    #[test]
    fn enforces_limits() {
        let xml = br#"<a><b><c/></b></a>"#;
        assert_eq!(parse_xml(xml, 4, 32), Err(XmlError::TooLarge));
        assert_eq!(parse_xml(xml, 1000, 2), Err(XmlError::TooDeep));
    }

    #[test]
    fn escapes_output() {
        assert_eq!(escape("<a&b>\"'"), "&lt;a&amp;b&gt;&quot;&apos;");
    }
}
