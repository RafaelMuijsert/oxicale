use std::collections::BTreeSet;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::extract::Request;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use oxicale_core::{
    normalize_path, CalendarComponent, CollectionKind, CollectionPath, ComponentSet, Href,
    ItemComponent, Permissions, UserName,
};
use oxicale_ical::filter::{
    ParamFilter, PropFilter, TextMatch as IcalTextMatch, TextMatchKind as IcalTextMatchKind,
};
use oxicale_ical::{CalendarFilter, ComponentFilter, IcalTimeRange, Limits as IcalLimits};
use oxicale_storage::{
    ChangeOperation, CollectionMeta, CollectionUpdate, Preconditions, StorageError, StoredItem,
};
use oxicale_vcard::{
    MatchTest, ParamFilter as VcardParamFilter, PropFilter as VcardPropFilter,
    TextMatch as VcardTextMatch, TextMatchKind as VcardTextMatchKind, VcardFilter, VcardLimits,
};
use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};

use crate::server::{peer_from_request, simple_response, AppState};
use crate::xml::{self, Element, XmlError, APPLE, CALDAV, CALENDARSERVER, CARDDAV, DAV, INFCLOUD};

const PATH_SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'/')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

const XML_LIMIT: usize = 1024 * 1024;
const XML_DEPTH: usize = 32;

#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
enum Resource {
    Root {
        user: UserName,
    },
    Principal {
        user: UserName,
        meta: oxicale_storage::PrincipalMeta,
    },
    Collection {
        current: UserName,
        path: CollectionPath,
        meta: CollectionMeta,
        permissions: Permissions,
    },
    Item {
        current: UserName,
        path: CollectionPath,
        item: StoredItem,
        permissions: Permissions,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct QName {
    namespace: String,
    local: String,
}

pub async fn handle_dav(state: Arc<AppState>, request: Request, segments: &[String]) -> Response {
    let method = request.method().clone();
    if method == Method::OPTIONS {
        return options_response(&state, segments);
    }

    let headers = request.headers().clone();
    let peer = peer_from_request(&request);
    let user = match state.authenticate(&headers, peer) {
        Ok(user) => user,
        Err(crate::auth::AuthError::RateLimited) => {
            return simple_response(
                StatusCode::TOO_MANY_REQUESTS,
                "text/plain",
                "too many requests",
            )
        }
        Err(crate::auth::AuthError::Unavailable(_)) => {
            return simple_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "text/plain",
                "authentication unavailable",
            )
        }
        Err(crate::auth::AuthError::Invalid) => return unauthorized(&state),
    };
    if let Err(error) = state.provision_account(&user).await {
        tracing::error!(%error, "failed to provision account");
        return simple_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "text/plain",
            "account provisioning failed",
        );
    }

    let (_parts, body) = request.into_parts();
    let body = match to_bytes(body, state.config.server.max_request_body.bytes().max(1024)).await {
        Ok(body) => body,
        Err(_) => {
            return simple_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "text/plain",
                "request too large",
            )
        }
    };

    match method.as_str() {
        "PROPFIND" => handle_propfind(&state, &user, segments, &headers, &body).await,
        "PROPPATCH" => handle_proppatch(&state, &user, segments, &headers, &body).await,
        "MKCOL" => handle_mkcol(&state, &user, segments, &headers, &body, false).await,
        "MKCALENDAR" => handle_mkcol(&state, &user, segments, &headers, &body, true).await,
        "GET" | "HEAD" => {
            handle_get_head(
                &state,
                &user,
                segments,
                &headers,
                &body,
                method == Method::HEAD,
            )
            .await
        }
        "PUT" => handle_put(&state, &user, segments, &headers, &body).await,
        "DELETE" => handle_delete(&state, &user, segments, &headers).await,
        "MOVE" => handle_move(&state, &user, segments, &headers).await,
        "REPORT" => handle_report(&state, &user, segments, &headers, &body).await,
        _ => method_not_allowed(allow_for(segments)),
    }
}

fn options_response(state: &AppState, segments: &[String]) -> Response {
    let allow = if segments.first().map(String::as_str) == Some("feeds") {
        "OPTIONS, GET, HEAD"
    } else {
        allow_for(segments)
    };
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::ALLOW, allow)
        .body(Body::empty())
        .unwrap();
    if state.config.addressbook.enabled {
        response.headers_mut().insert(
            axum::http::header::HeaderName::from_static("dav"),
            axum::http::HeaderValue::from_static(
                "1, 3, calendar-access, addressbook, sync-collection, extended-mkcol",
            ),
        );
    } else {
        response.headers_mut().insert(
            axum::http::header::HeaderName::from_static("dav"),
            axum::http::HeaderValue::from_static(
                "1, 3, calendar-access, sync-collection, extended-mkcol",
            ),
        );
    }
    response
}

fn allow_for(segments: &[String]) -> &'static str {
    match segments.len() {
        0 | 1 => "OPTIONS, GET, HEAD, PROPFIND",
        2 => "OPTIONS, GET, HEAD, PROPFIND, PROPPATCH, REPORT, DELETE, MOVE",
        _ => "OPTIONS, GET, HEAD, PROPFIND, PUT, DELETE, MOVE",
    }
}

fn unauthorized(state: &AppState) -> Response {
    let realm = state.auth.realm();
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header(
            axum::http::header::WWW_AUTHENTICATE,
            format!("Basic realm=\"{realm}\", charset=\"UTF-8\""),
        )
        .header(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )
        .body(Body::from("Unauthorized"))
        .unwrap()
}

fn method_not_allowed(allow: &str) -> Response {
    Response::builder()
        .status(StatusCode::METHOD_NOT_ALLOWED)
        .header(axum::http::header::ALLOW, allow)
        .body(Body::empty())
        .unwrap()
}

fn forbidden() -> Response {
    simple_response(
        StatusCode::FORBIDDEN,
        "text/plain; charset=utf-8",
        "Forbidden",
    )
}

fn not_found() -> Response {
    simple_response(
        StatusCode::NOT_FOUND,
        "text/plain; charset=utf-8",
        "Not Found",
    )
}

fn precondition_failed() -> Response {
    simple_response(
        StatusCode::PRECONDITION_FAILED,
        "text/plain; charset=utf-8",
        "Precondition Failed",
    )
}

fn dav_error(status: StatusCode, namespace: &str, local: &str) -> Response {
    let prefix = ns_prefix(namespace);
    let body = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:error xmlns:D=\"{DAV}\" xmlns:C=\"{CALDAV}\" xmlns:CR=\"{CARDDAV}\"><{}:{}/></D:error>",
        prefix, local
    );
    Response::builder()
        .status(status)
        .header(
            axum::http::header::CONTENT_TYPE,
            "application/xml; charset=utf-8",
        )
        .body(Body::from(body))
        .unwrap()
}

fn storage_error(error: StorageError) -> Response {
    match error {
        StorageError::NotFound(_) => not_found(),
        StorageError::Conflict(_) => dav_error(StatusCode::CONFLICT, CALDAV, "no-uid-conflict"),
        StorageError::PreconditionFailed => precondition_failed(),
        StorageError::ReadOnly => forbidden(),
        StorageError::Limit(_) => simple_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "text/plain",
            "item too large",
        ),
        StorageError::Ical(_) => dav_error(StatusCode::FORBIDDEN, CALDAV, "valid-calendar-data"),
        StorageError::Unsupported(_) => dav_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            CALDAV,
            "supported-calendar-data",
        ),
        StorageError::Vcard(_) => dav_error(StatusCode::FORBIDDEN, CARDDAV, "valid-address-data"),
        other => {
            tracing::error!(%other, "storage error");
            simple_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "text/plain",
                "storage error",
            )
        }
    }
}

fn resolve(
    state: &AppState,
    current: &UserName,
    segments: &[String],
) -> Result<Resource, Response> {
    match segments {
        [] => Ok(Resource::Root {
            user: current.clone(),
        }),
        [user] => {
            let target = UserName::parse(user.clone()).map_err(|_| not_found())?;
            let permissions = state.rights.permissions(current, &target);
            if !permissions.contains(Permissions::READ) {
                return Err(forbidden());
            }
            let meta = state.store.principal(&target).map_err(|_| not_found())?;
            Ok(Resource::Principal { user: target, meta })
        }
        [user, slug] => {
            let path = CollectionPath::parse(user, slug).map_err(|_| not_found())?;
            let meta = state.store.collection(&path).map_err(|_| not_found())?;
            let permissions = state.rights.permissions(current, &path.user);
            let permissions = if meta.is_read_only() {
                crate::rights::apply_read_only(permissions)
            } else {
                permissions
            };
            if !permissions.contains(Permissions::READ) {
                return Err(forbidden());
            }
            Ok(Resource::Collection {
                current: current.clone(),
                path,
                meta,
                permissions,
            })
        }
        [user, slug, href] => {
            let path = CollectionPath::parse(user, slug).map_err(|_| not_found())?;
            let meta = state.store.collection(&path).map_err(|_| not_found())?;
            let permissions = state.rights.permissions(current, &path.user);
            let permissions = if meta.is_read_only() {
                crate::rights::apply_read_only(permissions)
            } else {
                permissions
            };
            if !permissions.contains(Permissions::READ) {
                return Err(forbidden());
            }
            let href = Href::parse(href.clone()).map_err(|_| not_found())?;
            let item = state
                .store
                .get_item(&path, &href)
                .map_err(|_| not_found())?;
            Ok(Resource::Item {
                current: current.clone(),
                path,
                item,
                permissions,
            })
        }
        _ => Err(not_found()),
    }
}

fn href_for(state: &AppState, segments: &[String], trailing: bool) -> String {
    let base = state.config.server.normalized_base_path();
    let base = base.trim_end_matches('/');
    let encoded = segments
        .iter()
        .map(|segment| utf8_percent_encode(segment, PATH_SEGMENT).to_string())
        .collect::<Vec<_>>()
        .join("/");
    let mut href = if encoded.is_empty() {
        format!("{base}/")
    } else {
        format!("{base}/{encoded}")
    };
    if trailing && !href.ends_with('/') {
        href.push('/');
    }
    href
}

fn resource_href(state: &AppState, resource: &Resource) -> String {
    match resource {
        Resource::Root { .. } => href_for(state, &[], true),
        Resource::Principal { user, .. } => href_for(state, &[user.as_str().to_string()], true),
        Resource::Collection { path, .. } => href_for(
            state,
            &[path.user.as_str().into(), path.slug.as_str().into()],
            true,
        ),
        Resource::Item { path, item, .. } => href_for(
            state,
            &[
                path.user.as_str().into(),
                path.slug.as_str().into(),
                item.href.as_str().into(),
            ],
            false,
        ),
    }
}

fn ns_prefix(namespace: &str) -> &'static str {
    match namespace {
        DAV => "D",
        CALDAV => "C",
        CARDDAV => "CR",
        CALENDARSERVER => "CS",
        APPLE => "I",
        INFCLOUD => "INF",
        _ => "D",
    }
}

fn tag(namespace: &str, local: &str) -> String {
    format!("{}:{local}", ns_prefix(namespace))
}

fn default_props(resource: &Resource, addressbook_enabled: bool) -> Vec<QName> {
    let mut props = vec![
        QName {
            namespace: DAV.into(),
            local: "resourcetype".into(),
        },
        QName {
            namespace: DAV.into(),
            local: "displayname".into(),
        },
    ];
    props.push(QName {
        namespace: DAV.into(),
        local: "principal-collection-set".into(),
    });
    if !matches!(resource, Resource::Root { .. }) {
        props.push(QName {
            namespace: DAV.into(),
            local: "current-user-principal".into(),
        });
    }
    match resource {
        Resource::Root { .. } => props.push(QName {
            namespace: DAV.into(),
            local: "current-user-principal".into(),
        }),
        Resource::Principal { .. } => {
            props.extend([
                QName {
                    namespace: DAV.into(),
                    local: "principal-URL".into(),
                },
                QName {
                    namespace: CALDAV.into(),
                    local: "calendar-home-set".into(),
                },
                QName {
                    namespace: CALDAV.into(),
                    local: "calendar-user-address-set".into(),
                },
            ]);
            if addressbook_enabled {
                props.push(QName {
                    namespace: CARDDAV.into(),
                    local: "addressbook-home-set".into(),
                });
            }
        }
        Resource::Collection { meta, .. } => {
            props.extend([
                QName {
                    namespace: DAV.into(),
                    local: "owner".into(),
                },
                QName {
                    namespace: DAV.into(),
                    local: "current-user-privilege-set".into(),
                },
                QName {
                    namespace: DAV.into(),
                    local: "supported-report-set".into(),
                },
                QName {
                    namespace: DAV.into(),
                    local: "sync-token".into(),
                },
                QName {
                    namespace: CALENDARSERVER.into(),
                    local: "getctag".into(),
                },
                QName {
                    namespace: DAV.into(),
                    local: "getlastmodified".into(),
                },
                QName {
                    namespace: DAV.into(),
                    local: "getetag".into(),
                },
                QName {
                    namespace: DAV.into(),
                    local: "getcontenttype".into(),
                },
            ]);
            if meta.kind == CollectionKind::Calendar {
                props.extend([
                    QName {
                        namespace: CALDAV.into(),
                        local: "supported-calendar-component-set".into(),
                    },
                    QName {
                        namespace: CALDAV.into(),
                        local: "calendar-description".into(),
                    },
                    QName {
                        namespace: APPLE.into(),
                        local: "calendar-color".into(),
                    },
                    QName {
                        namespace: CALDAV.into(),
                        local: "max-resource-size".into(),
                    },
                    QName {
                        namespace: CALDAV.into(),
                        local: "supported-calendar-data".into(),
                    },
                ]);
                if meta.subscription.is_some() {
                    props.push(QName {
                        namespace: CALENDARSERVER.into(),
                        local: "source".into(),
                    });
                }
            } else {
                props.extend([
                    QName {
                        namespace: CARDDAV.into(),
                        local: "addressbook-description".into(),
                    },
                    QName {
                        namespace: INFCLOUD.into(),
                        local: "addressbook-color".into(),
                    },
                    QName {
                        namespace: CARDDAV.into(),
                        local: "max-resource-size".into(),
                    },
                    QName {
                        namespace: CARDDAV.into(),
                        local: "supported-address-data".into(),
                    },
                ]);
            }
        }
        Resource::Item { .. } => {
            props.extend([
                QName {
                    namespace: DAV.into(),
                    local: "getetag".into(),
                },
                QName {
                    namespace: DAV.into(),
                    local: "getcontenttype".into(),
                },
                QName {
                    namespace: DAV.into(),
                    local: "getcontentlength".into(),
                },
                QName {
                    namespace: DAV.into(),
                    local: "getlastmodified".into(),
                },
                QName {
                    namespace: DAV.into(),
                    local: "creationdate".into(),
                },
            ]);
        }
    }
    props
}

fn property_inner(state: &AppState, resource: &Resource, qname: &QName) -> Option<String> {
    let local = qname.local.as_str();
    let namespace = qname.namespace.as_str();
    let current_href = resource_href(state, resource);
    match (namespace, local) {
        (DAV, "resourcetype") => Some(match resource {
            Resource::Root { .. } => "<D:collection/>".to_string(),
            Resource::Principal { .. } => "<D:collection/><D:principal/>".to_string(),
            Resource::Collection { meta, .. } => {
                let mut value = "<D:collection/>".to_string();
                match meta.kind {
                    CollectionKind::Calendar => value.push_str("<C:calendar/>"),
                    CollectionKind::AddressBook => value.push_str("<CR:addressbook/>"),
                }
                if meta.subscription.is_some() {
                    value.push_str("<CS:subscribed/>");
                }
                value
            }
            Resource::Item { .. } => String::new(),
        }),
        (DAV, "displayname") => Some(xml::escape(match resource {
            Resource::Root { .. } => "Oxicale",
            Resource::Principal { meta, .. } => {
                meta.display_name.as_deref().unwrap_or(&meta.username)
            }
            Resource::Collection { meta, .. } => &meta.display_name,
            Resource::Item { item, .. } => item.href.as_str(),
        })),
        (DAV, "current-user-principal") => {
            let user = match resource {
                Resource::Root { user }
                | Resource::Principal { user, .. }
                | Resource::Collection { current: user, .. }
                | Resource::Item { current: user, .. } => user,
            };
            Some(format!(
                "<D:href>{}</D:href>",
                xml::escape(&href_for(state, &[user.as_str().into()], true))
            ))
        }
        (DAV, "principal-collection-set") => Some(format!(
            "<D:href>{}</D:href>",
            xml::escape(&href_for(state, &[], true))
        )),
        (DAV, "principal-URL") => match resource {
            Resource::Principal { user, .. } => Some(format!(
                "<D:href>{}</D:href>",
                xml::escape(&href_for(state, &[user.as_str().into()], true))
            )),
            _ => None,
        },
        (CALDAV, "calendar-home-set") => match resource {
            Resource::Principal { user, .. } => Some(format!(
                "<D:href>{}</D:href>",
                xml::escape(&href_for(state, &[user.as_str().into()], true))
            )),
            _ => None,
        },
        (CARDDAV, "addressbook-home-set") => match resource {
            Resource::Principal { user, .. } => Some(format!(
                "<D:href>{}</D:href>",
                xml::escape(&href_for(state, &[user.as_str().into()], true))
            )),
            _ => None,
        },
        (CALDAV, "calendar-user-address-set") => match resource {
            Resource::Principal { user, .. } => Some(format!(
                "<D:href>{}</D:href>",
                xml::escape(&href_for(state, &[user.as_str().into()], true))
            )),
            _ => None,
        },
        (DAV, "owner") => match resource {
            Resource::Collection { path, .. } => Some(format!(
                "<D:href>{}</D:href>",
                xml::escape(&href_for(state, &[path.user.as_str().into()], true))
            )),
            _ => None,
        },
        (DAV, "getetag") => match resource {
            Resource::Item { item, .. } => {
                Some(xml::escape(&format!("\"{}\"", item.etag)))
            }
            Resource::Collection { meta, .. } => {
                Some(xml::escape(&format!("\"{}\"", state.store.ctag(meta))))
            }
            _ => None,
        },
        (DAV, "getcontenttype") => match resource {
            Resource::Item { item, .. } => Some(item_content_type(item.component)),
            Resource::Collection { meta, .. } => Some(
                match meta.kind {
                    CollectionKind::Calendar => "text/calendar; charset=utf-8",
                    CollectionKind::AddressBook => "text/vcard; charset=utf-8",
                }
                .to_string(),
            ),
            _ => None,
        },
        (DAV, "getcontentlength") => match resource {
            Resource::Item { item, .. } => Some(item.size.to_string()),
            _ => None,
        },
        (DAV, "getlastmodified") => {
            let updated = match resource {
                Resource::Collection { meta, .. } => Some(meta.updated_at),
                Resource::Item { item, .. } => Some(item.updated_at),
                _ => None,
            };
            updated.map(|instant| xml::escape(&httpdate::fmt_http_date(instant.into())))
        }
        (DAV, "creationdate") => match resource {
            Resource::Collection { meta, .. } => Some(xml::escape(&meta.created_at.to_rfc3339())),
            Resource::Item { item, .. } => Some(xml::escape(&item.updated_at.to_rfc3339())),
            _ => None,
        },
        (DAV, "current-user-privilege-set") => match resource {
            Resource::Collection { permissions, .. } | Resource::Item { permissions, .. } => {
                Some(serialize_privileges(*permissions))
            }
            _ => None,
        },
        (DAV, "supported-report-set") => match resource {
            Resource::Collection { meta, .. } => Some(serialize_reports(meta.kind)),
            _ => None,
        },
        (DAV, "sync-token") => match resource {
            Resource::Collection { path, .. } => state
                .store
                .sync_token(path)
                .ok()
                .map(|token| xml::escape(&token)),
            _ => None,
        },
        (CALENDARSERVER, "getctag") => match resource {
            Resource::Collection { meta, .. } => {
                Some(xml::escape(&state.store.ctag(meta)))
            }
            _ => None,
        },
        (CALDAV, "supported-calendar-component-set") => match resource {
            Resource::Collection { meta, .. } if meta.kind == CollectionKind::Calendar => {
                Some(
                    CalendarComponent::ALL
                        .iter()
                        .map(|component| format!("<C:comp name=\"{}\"/>", component.as_str()))
                        .collect(),
                )
            }
            _ => None,
        },
        (CALDAV, "calendar-description") => match resource {
            Resource::Collection { meta, .. } if meta.kind == CollectionKind::Calendar => {
                Some(xml::escape(meta.description.as_deref().unwrap_or("")))
            }
            _ => None,
        },
        (CARDDAV, "addressbook-description") => match resource {
            Resource::Collection { meta, .. } if meta.kind == CollectionKind::AddressBook => {
                Some(xml::escape(meta.description.as_deref().unwrap_or("")))
            }
            _ => None,
        },
        (APPLE, "calendar-color") => match resource {
            Resource::Collection { meta, .. } if meta.kind == CollectionKind::Calendar => {
                Some(xml::escape(meta.color.as_deref().unwrap_or("")))
            }
            _ => None,
        },
        (INFCLOUD, "addressbook-color") => match resource {
            Resource::Collection { meta, .. } if meta.kind == CollectionKind::AddressBook => {
                Some(xml::escape(meta.color.as_deref().unwrap_or("")))
            }
            _ => None,
        },
        (CALDAV, "max-resource-size") => match resource {
            Resource::Collection { meta, .. } if meta.kind == CollectionKind::Calendar => {
                Some(state.config.server.max_item_size.bytes().to_string())
            }
            _ => None,
        },
        (CARDDAV, "max-resource-size") => match resource {
            Resource::Collection { meta, .. } if meta.kind == CollectionKind::AddressBook => {
                Some(state.config.server.max_addressbook_item_size.bytes().to_string())
            }
            _ => None,
        },
        (CALDAV, "supported-calendar-data") => match resource {
            Resource::Collection { meta, .. } if meta.kind == CollectionKind::Calendar => {
                Some("<C:calendar-data content-type=\"text/calendar\" version=\"2.0\"/>".into())
            }
            _ => None,
        },
        (CARDDAV, "supported-address-data") => match resource {
            Resource::Collection { meta, .. } if meta.kind == CollectionKind::AddressBook => {
                Some(
                    "<CR:address-data-type content-type=\"text/vcard\" version=\"3.0\"/><CR:address-data-type content-type=\"text/vcard\" version=\"4.0\"/>".into(),
                )
            }
            _ => None,
        },
        (CALENDARSERVER, "source") => match resource {
            Resource::Collection { meta, .. } => meta
                .subscription
                .as_ref()
                .map(|source| format!("<D:href>{}</D:href>", xml::escape(&source.url))),
            _ => None,
        },
        _ => {
            let _ = current_href;
            None
        }
    }
}

fn serialize_privileges(permissions: Permissions) -> String {
    let mut out = String::new();
    let mut push = |local: &str| {
        out.push_str(&format!("<D:{local}/>"));
    };
    if permissions.contains(Permissions::READ) {
        push("read");
    }
    if permissions.contains(Permissions::WRITE) {
        push("write");
        push("write-content");
        push("bind");
    }
    if permissions.contains(Permissions::DELETE) {
        push("unbind");
    }
    if permissions.contains(Permissions::WRITE_PROPERTIES) {
        push("write-properties");
    }
    if permissions.contains(Permissions::READ_FREEBUSY) {
        push("read-free-busy");
    }
    out
}

fn serialize_reports(kind: CollectionKind) -> String {
    let mut out = String::new();
    match kind {
        CollectionKind::Calendar => {
            for report in ["calendar-query", "calendar-multiget", "free-busy-query"] {
                out.push_str(&format!(
                    "<D:supported-report><D:report><C:{report}/></D:report></D:supported-report>"
                ));
            }
        }
        CollectionKind::AddressBook => {
            for report in ["addressbook-query", "addressbook-multiget"] {
                out.push_str(&format!(
                    "<D:supported-report><D:report><CR:{report}/></D:report></D:supported-report>"
                ));
            }
        }
    }
    out.push_str(
        "<D:supported-report><D:report><D:sync-collection/></D:report></D:supported-report>",
    );
    out
}

fn item_content_type(component: ItemComponent) -> String {
    match component {
        ItemComponent::VCard => "text/vcard; charset=utf-8".to_string(),
        other => format!("text/calendar; charset=utf-8; component={}", other.as_str()),
    }
}

fn parse_propfind(body: &[u8]) -> Result<(Vec<QName>, bool, bool), Response> {
    if body.is_empty() {
        return Ok((Vec::new(), true, false));
    }
    let root = xml::parse_xml(body, XML_LIMIT, XML_DEPTH).map_err(xml_error)?;
    if root.local != "propfind" {
        return Err(bad_request("expected propfind"));
    }
    if root.find("allprop").is_some() {
        return Ok((Vec::new(), true, false));
    }
    if root.find("propname").is_some() {
        return Ok((Vec::new(), false, true));
    }
    let prop = root
        .find("prop")
        .ok_or_else(|| bad_request("missing prop"))?;
    let names = prop
        .children
        .iter()
        .map(|child| QName {
            namespace: child.namespace.clone(),
            local: child.local.clone(),
        })
        .collect();
    Ok((names, false, false))
}

async fn handle_propfind(
    state: &Arc<AppState>,
    user: &UserName,
    segments: &[String],
    headers: &HeaderMap,
    body: &[u8],
) -> Response {
    let depth = headers
        .get("Depth")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("infinity");
    if depth == "infinity" {
        return simple_response(
            StatusCode::FORBIDDEN,
            "text/plain",
            "Depth: infinity is not supported",
        );
    }
    if depth != "0" && depth != "1" {
        return bad_request("invalid Depth");
    }
    let (requested, allprop, propname) = match parse_propfind(body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let resource = match resolve(state, user, segments) {
        Ok(resource) => resource,
        Err(response) => return response,
    };
    let mut resources = vec![resource.clone()];
    if depth == "1" {
        match &resource {
            Resource::Root { user } => {
                if let Ok(meta) = state.store.principal(user) {
                    resources.push(Resource::Principal {
                        user: user.clone(),
                        meta,
                    });
                }
            }
            Resource::Principal { user, .. } => {
                if let Ok(collections) = state.store.list_collections(user) {
                    for meta in collections {
                        let path = CollectionPath::parse(user.as_str(), &meta.slug).unwrap();
                        let permissions = state.rights.permissions(user, user);
                        let permissions = if meta.is_read_only() {
                            crate::rights::apply_read_only(permissions)
                        } else {
                            permissions
                        };
                        resources.push(Resource::Collection {
                            current: user.clone(),
                            path,
                            meta,
                            permissions,
                        });
                    }
                }
            }
            Resource::Collection {
                path, permissions, ..
            } => {
                if let Ok(items) = state.store.list_items(path) {
                    for item in items {
                        resources.push(Resource::Item {
                            current: user.clone(),
                            path: path.clone(),
                            item,
                            permissions: *permissions,
                        });
                    }
                }
            }
            Resource::Item { .. } => {}
        }
    }

    let mut xml_body = response_start();
    for resource in resources {
        let props = if allprop {
            default_props(&resource, state.config.addressbook.enabled)
        } else {
            requested.clone()
        };
        let (found, missing) = if propname {
            (
                props.iter().map(property_tag_element).collect::<String>(),
                Vec::new(),
            )
        } else {
            let mut found = String::new();
            let mut missing = Vec::new();
            for qname in &props {
                match property_inner(state, &resource, qname) {
                    Some(value) => {
                        found.push_str(&format!(
                            "<{tag}>{value}</{tag}>",
                            tag = tag(&qname.namespace, &qname.local)
                        ));
                    }
                    None => missing.push(qname.clone()),
                }
            }
            (found, missing)
        };
        xml_body.push_str(&response_for_resource(
            &resource_href(state, &resource),
            &found,
            &missing,
        ));
    }
    xml_body.push_str("</D:multistatus>");
    xml_response(StatusCode::MULTI_STATUS, xml_body)
}

fn property_tag_element(qname: &QName) -> String {
    let tag = tag(&qname.namespace, &qname.local);
    format!("<{tag}/>")
}

fn response_start() -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<D:multistatus xmlns:D=\"{DAV}\" xmlns:C=\"{CALDAV}\" xmlns:CR=\"{CARDDAV}\" xmlns:CS=\"{CALENDARSERVER}\" xmlns:I=\"{APPLE}\" xmlns:INF=\"{INFCLOUD}\">"
    )
}

fn response_for_resource(href: &str, found: &str, missing: &[QName]) -> String {
    let mut out = format!(
        "<D:response><D:href>{}</D:href><D:propstat><D:prop>{found}</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>",
        xml::escape(href)
    );
    if !missing.is_empty() {
        let mut missing_xml = String::new();
        for qname in missing {
            missing_xml.push_str(&property_tag_element(qname));
        }
        out.push_str(&format!(
            "<D:propstat><D:prop>{missing_xml}</D:prop><D:status>HTTP/1.1 404 Not Found</D:status></D:propstat>"
        ));
    }
    out.push_str("</D:response>");
    out
}

fn xml_response(status: StatusCode, body: String) -> Response {
    Response::builder()
        .status(status)
        .header(
            axum::http::header::CONTENT_TYPE,
            "application/xml; charset=utf-8",
        )
        .header(axum::http::header::CACHE_CONTROL, "no-cache")
        .body(Body::from(body))
        .unwrap()
}

fn xml_error(error: XmlError) -> Response {
    let status = match error {
        XmlError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        _ => StatusCode::BAD_REQUEST,
    };
    simple_response(status, "text/plain; charset=utf-8", "invalid XML")
}

fn bad_request(message: &str) -> Response {
    simple_response(
        StatusCode::BAD_REQUEST,
        "text/plain; charset=utf-8",
        message,
    )
}

async fn handle_proppatch(
    state: &Arc<AppState>,
    user: &UserName,
    segments: &[String],
    _headers: &HeaderMap,
    body: &[u8],
) -> Response {
    let resource = match resolve(state, user, segments) {
        Ok(resource) => resource,
        Err(response) => return response,
    };
    let (current, path, meta, permissions) = match &resource {
        Resource::Collection {
            current,
            path,
            meta,
            permissions,
        } => (current.clone(), path.clone(), meta.clone(), *permissions),
        _ => return method_not_allowed(allow_for(segments)),
    };
    if !permissions.contains(Permissions::WRITE_PROPERTIES) {
        return forbidden();
    }
    let root = match xml::parse_xml(body, XML_LIMIT, XML_DEPTH) {
        Ok(root) => root,
        Err(error) => return xml_error(error),
    };
    if root.local != "propertyupdate" {
        return bad_request("expected propertyupdate");
    }
    let mut update = CollectionUpdate::default();
    let mut statuses: Vec<(QName, bool)> = Vec::new();
    for action in &root.children {
        let set = match action.local.as_str() {
            "set" => true,
            "remove" => false,
            _ => continue,
        };
        let Some(prop) = action.find("prop") else {
            continue;
        };
        for property in &prop.children {
            let qname = QName {
                namespace: property.namespace.clone(),
                local: property.local.clone(),
            };
            let allowed = match (qname.namespace.as_str(), qname.local.as_str()) {
                (DAV, "displayname") => {
                    update.display_name = Some(if set {
                        property.text_trimmed().to_string()
                    } else {
                        meta.slug.clone()
                    });
                    true
                }
                (CALDAV, "calendar-description") if meta.kind == CollectionKind::Calendar => {
                    if set {
                        update.description = Some(property.text_trimmed().to_string());
                    } else {
                        update.clear_description = true;
                    }
                    true
                }
                (CARDDAV, "addressbook-description")
                    if meta.kind == CollectionKind::AddressBook =>
                {
                    if set {
                        update.description = Some(property.text_trimmed().to_string());
                    } else {
                        update.clear_description = true;
                    }
                    true
                }
                (APPLE, "calendar-color") if meta.kind == CollectionKind::Calendar => {
                    if set {
                        update.color = Some(property.text_trimmed().to_string());
                    } else {
                        update.clear_color = true;
                    }
                    true
                }
                (INFCLOUD, "addressbook-color") if meta.kind == CollectionKind::AddressBook => {
                    if set {
                        update.color = Some(property.text_trimmed().to_string());
                    } else {
                        update.clear_color = true;
                    }
                    true
                }
                (DAV, "resourcetype")
                | (CALDAV, "supported-calendar-component-set")
                | (CALDAV, "calendar-timezone") => false,
                _ => false,
            };
            statuses.push((qname, allowed));
        }
    }
    if statuses.iter().any(|(_, allowed)| !*allowed) {
        let mut out = response_start();
        let mut ok = String::new();
        let mut denied = String::new();
        for (qname, allowed) in &statuses {
            let element = property_tag_element(qname);
            if *allowed {
                ok.push_str(&element);
            } else {
                denied.push_str(&element);
            }
        }
        let href = resource_href(state, &resource);
        out.push_str(&format!(
            "<D:response><D:href>{}</D:href>",
            xml::escape(&href)
        ));
        if !ok.is_empty() {
            out.push_str(&format!(
                "<D:propstat><D:prop>{ok}</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat>"
            ));
        }
        if !denied.is_empty() {
            out.push_str(&format!(
                "<D:propstat><D:prop>{denied}</D:prop><D:status>HTTP/1.1 403 Forbidden</D:status></D:propstat>"
            ));
        }
        out.push_str("</D:response></D:multistatus>");
        return xml_response(StatusCode::MULTI_STATUS, out);
    }
    if meta.subscription.is_some() {
        // Content-affecting properties are rejected above; local metadata is
        // allowed on read-only subscription collections.
    }
    let updated = match state.store.update_collection(&path, update).await {
        Ok(updated) => updated,
        Err(error) => return storage_error(error),
    };
    let mut out = response_start();
    let mut props = String::new();
    for (qname, _) in &statuses {
        props.push_str(&property_tag_element(qname));
    }
    out.push_str(&format!(
        "<D:response><D:href>{}</D:href><D:propstat><D:prop>{props}</D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response></D:multistatus>",
        xml::escape(&resource_href(
            state,
            &Resource::Collection {
                current,
                path: path.clone(),
                meta: updated,
                permissions,
            }
        ))
    ));
    xml_response(StatusCode::MULTI_STATUS, out)
}

async fn handle_mkcol(
    state: &Arc<AppState>,
    user: &UserName,
    segments: &[String],
    _headers: &HeaderMap,
    body: &[u8],
    mkcalendar: bool,
) -> Response {
    let (target_user, slug) = match segments {
        [target_user, slug] => (target_user, slug),
        _ => return method_not_allowed(allow_for(segments)),
    };
    if target_user != user.as_str() {
        return forbidden();
    }
    let path = match CollectionPath::parse(target_user, slug) {
        Ok(path) => path,
        Err(_) => return bad_request("invalid collection path"),
    };
    if state.store.collection(&path).is_ok() {
        return method_not_allowed(allow_for(segments));
    }
    let mut kind = if mkcalendar {
        CollectionKind::Calendar
    } else {
        CollectionKind::AddressBook
    };
    let mut display_name = slug.clone();

    if !body.is_empty() {
        let root = match xml::parse_xml(body, XML_LIMIT, XML_DEPTH) {
            Ok(root) => root,
            Err(error) => return xml_error(error),
        };
        if let Some(prop) = root
            .find("prop")
            .or_else(|| root.find("set").and_then(|s| s.find("prop")))
        {
            for property in &prop.children {
                match (property.namespace.as_str(), property.local.as_str()) {
                    (DAV, "displayname") => {
                        display_name = property.text_trimmed().to_string();
                    }
                    (DAV, "resourcetype") => {
                        if !mkcalendar {
                            if property.child(CALDAV, "calendar").is_some() {
                                kind = CollectionKind::Calendar;
                            } else if property.child(CARDDAV, "addressbook").is_some() {
                                kind = CollectionKind::AddressBook;
                            }
                        }
                    }
                    (CALDAV, "supported-calendar-component-set") => {
                        for comp in property.children_named(CALDAV, "comp") {
                            let name = comp.attr("name").unwrap_or_default();
                            if name.parse::<CalendarComponent>().is_err() {
                                return dav_error(
                                    StatusCode::FORBIDDEN,
                                    CALDAV,
                                    "supported-calendar-component",
                                );
                            }
                        }
                        // Narrowing is intentionally ignored: calendars are mixed.
                    }
                    _ => {}
                }
            }
        }
    }
    if kind == CollectionKind::Calendar && !state.config.calendar.allow_client_creation {
        return forbidden();
    }
    if kind == CollectionKind::AddressBook && !state.config.addressbook.enabled {
        return forbidden();
    }
    match state
        .store
        .create_collection(user, kind, slug, &display_name)
        .await
    {
        Ok(_) => {
            let mut response = Response::builder()
                .status(StatusCode::CREATED)
                .body(Body::empty())
                .unwrap();
            response.headers_mut().insert(
                axum::http::header::LOCATION,
                axum::http::HeaderValue::from_str(&href_for(
                    state,
                    &[user.as_str().into(), slug.clone()],
                    true,
                ))
                .unwrap_or_else(|_| axum::http::HeaderValue::from_static("/")),
            );
            response
        }
        Err(error) => storage_error(error),
    }
}

async fn handle_get_head(
    state: &Arc<AppState>,
    user: &UserName,
    segments: &[String],
    headers: &HeaderMap,
    _body: &[u8],
    head: bool,
) -> Response {
    if segments.is_empty() {
        return simple_response(
            StatusCode::OK,
            "text/plain; charset=utf-8",
            "Oxicale CalDAV/CardDAV server",
        );
    }
    let resource = match resolve(state, user, segments) {
        Ok(resource) => resource,
        Err(response) => return response,
    };
    match &resource {
        Resource::Item { item, .. } => {
            if conditional_not_modified(headers, &item.etag, item.updated_at) {
                return not_modified_response(&item.etag, item.updated_at);
            }
            let mut response = Response::builder()
                .status(StatusCode::OK)
                .header(
                    axum::http::header::CONTENT_TYPE,
                    item_content_type(item.component),
                )
                .header(axum::http::header::ETAG, format!("\"{}\"", item.etag))
                .header(
                    axum::http::header::LAST_MODIFIED,
                    httpdate::fmt_http_date(item.updated_at.into()),
                )
                .header(axum::http::header::CACHE_CONTROL, "no-cache")
                .body(if head {
                    Body::empty()
                } else {
                    Body::from(item.payload.clone())
                })
                .unwrap();
            response.headers_mut().insert(
                axum::http::header::CONTENT_LENGTH,
                axum::http::HeaderValue::from_str(&item.size.to_string())
                    .unwrap_or_else(|_| axum::http::HeaderValue::from_static("0")),
            );
            response
        }
        Resource::Collection { path, meta, .. } => {
            let items = match state.store.list_items(path) {
                Ok(items) => items,
                Err(error) => return storage_error(error),
            };
            let ctag = state.store.ctag(meta);
            if conditional_not_modified(headers, &ctag, meta.updated_at) {
                return not_modified_response(&ctag, meta.updated_at);
            }
            let (content_type, body) = match meta.kind {
                CollectionKind::Calendar => {
                    let payloads: Vec<String> =
                        items.into_iter().map(|item| item.payload).collect();
                    let prodid = format!("-//Oxicale//Oxicale {}//EN", state.version);
                    match oxicale_ical::generate_feed(
                        &payloads,
                        ComponentSet::all(),
                        Some(&meta.display_name),
                        &prodid,
                        IcalLimits {
                            max_bytes: state.store.options().max_calendar_item_size,
                            ..IcalLimits::default()
                        },
                    ) {
                        Ok(body) => ("text/calendar; charset=utf-8", body),
                        Err(_) => {
                            return simple_response(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "text/plain",
                                "cannot build calendar export",
                            )
                        }
                    }
                }
                CollectionKind::AddressBook => {
                    let body = items
                        .into_iter()
                        .map(|item| item.payload)
                        .collect::<String>();
                    ("text/vcard; charset=utf-8", body)
                }
            };
            let body_len = body.len();
            Response::builder()
                .status(StatusCode::OK)
                .header(axum::http::header::CONTENT_TYPE, content_type)
                .header(axum::http::header::ETAG, format!("\"{ctag}\""))
                .header(axum::http::header::CACHE_CONTROL, "no-cache")
                .header(axum::http::header::CONTENT_LENGTH, body_len.to_string())
                .header(
                    axum::http::header::CONTENT_DISPOSITION,
                    format!(
                        "inline; filename=\"{}.{}\"",
                        meta.slug,
                        if meta.kind == CollectionKind::Calendar {
                            "ics"
                        } else {
                            "vcf"
                        }
                    ),
                )
                .body(if head {
                    Body::empty()
                } else {
                    Body::from(body)
                })
                .unwrap()
        }
        Resource::Principal { .. } | Resource::Root { .. } => {
            simple_response(StatusCode::OK, "text/plain; charset=utf-8", "Oxicale")
        }
    }
}

fn conditional_not_modified(headers: &HeaderMap, etag: &str, updated_at: DateTime<Utc>) -> bool {
    if let Some(value) = headers
        .get(axum::http::header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    {
        return etag_matches(value, etag);
    }
    if let Some(value) = headers
        .get(axum::http::header::IF_MODIFIED_SINCE)
        .and_then(|value| value.to_str().ok())
    {
        if let Ok(since) = httpdate::parse_http_date(value) {
            let since: DateTime<Utc> = since.into();
            return updated_at <= since + chrono::Duration::seconds(1);
        }
    }
    false
}

fn not_modified_response(etag: &str, updated_at: DateTime<Utc>) -> Response {
    Response::builder()
        .status(StatusCode::NOT_MODIFIED)
        .header(axum::http::header::ETAG, format!("\"{etag}\""))
        .header(
            axum::http::header::LAST_MODIFIED,
            httpdate::fmt_http_date(updated_at.into()),
        )
        .body(Body::empty())
        .unwrap()
}

fn etag_matches(header: &str, etag: &str) -> bool {
    header.trim() == "*"
        || header.split(',').any(|candidate| {
            let candidate = candidate.trim();
            let candidate = candidate.strip_prefix("W/").unwrap_or(candidate);
            candidate.trim_matches('"') == etag
        })
}

async fn handle_put(
    state: &Arc<AppState>,
    user: &UserName,
    segments: &[String],
    headers: &HeaderMap,
    body: &[u8],
) -> Response {
    let (path, href) = match segments {
        [target_user, slug, href] => {
            if target_user != user.as_str() {
                return forbidden();
            }
            let path = match CollectionPath::parse(target_user, slug) {
                Ok(path) => path,
                Err(_) => return not_found(),
            };
            let href = match Href::parse(href.clone()) {
                Ok(href) => href,
                Err(_) => return bad_request("invalid href"),
            };
            (path, href)
        }
        _ => return method_not_allowed(allow_for(segments)),
    };
    let meta = match state.store.collection(&path) {
        Ok(meta) => meta,
        Err(_) => return not_found(),
    };
    let permissions = state.rights.permissions(user, &path.user);
    let permissions = if meta.is_read_only() {
        crate::rights::apply_read_only(permissions)
    } else {
        permissions
    };
    if !permissions.contains(Permissions::WRITE) {
        return forbidden();
    }
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let acceptable = match meta.kind {
        CollectionKind::Calendar => {
            content_type.is_empty()
                || content_type
                    .to_ascii_lowercase()
                    .starts_with("text/calendar")
        }
        CollectionKind::AddressBook => {
            let lower = content_type.to_ascii_lowercase();
            lower.is_empty() || lower.starts_with("text/vcard") || lower.starts_with("text/x-vcard")
        }
    };
    if !acceptable {
        return simple_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "text/plain",
            "unsupported content type",
        );
    }
    let payload = match std::str::from_utf8(body) {
        Ok(payload) => payload,
        Err(_) => return bad_request("request body is not UTF-8"),
    };
    let existing = state.store.get_item(&path, &href).ok();
    if let Some(existing) = &existing {
        if let Some(value) = headers
            .get(axum::http::header::IF_MATCH)
            .and_then(|value| value.to_str().ok())
        {
            if !etag_matches(value, &existing.etag) {
                return precondition_failed();
            }
        }
        if let Some(value) = headers
            .get(axum::http::header::IF_NONE_MATCH)
            .and_then(|value| value.to_str().ok())
        {
            if etag_matches(value, &existing.etag) {
                return precondition_failed();
            }
        }
    } else if headers
        .get(axum::http::header::IF_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some()
    {
        return precondition_failed();
    }
    let if_match = headers
        .get(axum::http::header::IF_MATCH)
        .and_then(|value| value.to_str().ok());
    let if_none_match = headers
        .get(axum::http::header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim() == "*");
    match state
        .store
        .put_item(
            &path,
            &href,
            payload,
            true,
            Preconditions {
                if_match,
                if_none_match,
            },
        )
        .await
    {
        Ok(outcome) => {
            let status = if outcome.created {
                StatusCode::CREATED
            } else {
                StatusCode::NO_CONTENT
            };
            let mut response = Response::builder()
                .status(status)
                .header(axum::http::header::ETAG, format!("\"{}\"", outcome.etag))
                .header(axum::http::header::CACHE_CONTROL, "no-cache")
                .body(Body::empty())
                .unwrap();
            if outcome.created {
                response.headers_mut().insert(
                    axum::http::header::LOCATION,
                    axum::http::HeaderValue::from_str(&href_for(
                        state,
                        &[
                            user.as_str().into(),
                            path.slug.as_str().into(),
                            href.as_str().into(),
                        ],
                        false,
                    ))
                    .unwrap_or_else(|_| axum::http::HeaderValue::from_static("/")),
                );
            }
            response
        }
        Err(error) => storage_error(error),
    }
}

async fn handle_delete(
    state: &Arc<AppState>,
    user: &UserName,
    segments: &[String],
    headers: &HeaderMap,
) -> Response {
    let if_match = headers
        .get(axum::http::header::IF_MATCH)
        .and_then(|value| value.to_str().ok());
    match segments {
        [target_user, slug, href] => {
            if target_user != user.as_str() {
                return forbidden();
            }
            let path = match CollectionPath::parse(target_user, slug) {
                Ok(path) => path,
                Err(_) => return not_found(),
            };
            let meta = match state.store.collection(&path) {
                Ok(meta) => meta,
                Err(_) => return not_found(),
            };
            let permissions = state.rights.permissions(user, &path.user);
            let permissions = if meta.is_read_only() {
                crate::rights::apply_read_only(permissions)
            } else {
                permissions
            };
            if !permissions.contains(Permissions::DELETE) {
                return forbidden();
            }
            let href = match Href::parse(href.clone()) {
                Ok(href) => href,
                Err(_) => return not_found(),
            };
            match state.store.delete_item(&path, &href, if_match).await {
                Ok(_) => Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .body(Body::empty())
                    .unwrap(),
                Err(error) => storage_error(error),
            }
        }
        [target_user, slug] => {
            if target_user != user.as_str() {
                return forbidden();
            }
            let path = match CollectionPath::parse(target_user, slug) {
                Ok(path) => path,
                Err(_) => return not_found(),
            };
            let permissions = state.rights.permissions(user, &path.user);
            if !permissions.contains(Permissions::DELETE) {
                return forbidden();
            }
            match state.store.delete_collection(&path, if_match).await {
                Ok(()) => Response::builder()
                    .status(StatusCode::NO_CONTENT)
                    .body(Body::empty())
                    .unwrap(),
                Err(error) => storage_error(error),
            }
        }
        _ => method_not_allowed(allow_for(segments)),
    }
}

async fn handle_move(
    state: &Arc<AppState>,
    user: &UserName,
    segments: &[String],
    headers: &HeaderMap,
) -> Response {
    let overwrite = headers
        .get("Overwrite")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.eq_ignore_ascii_case("T"))
        .unwrap_or(false);
    let destination = match headers
        .get("Destination")
        .and_then(|value| value.to_str().ok())
    {
        Some(value) => value,
        None => return bad_request("missing Destination"),
    };
    let destination_path = match url::Url::parse(destination) {
        Ok(url) => {
            if let (Some(host), Some(header_host)) = (
                url.host_str(),
                headers.get("Host").and_then(|value| value.to_str().ok()),
            ) {
                let header_host = header_host.split(':').next().unwrap_or(header_host);
                if !host.eq_ignore_ascii_case(header_host) {
                    return simple_response(
                        StatusCode::BAD_GATEWAY,
                        "text/plain",
                        "remote destination is not supported",
                    );
                }
            }
            url.path().to_string()
        }
        Err(_) => destination.to_string(),
    };
    let base = state.config.server.normalized_base_path();
    let base_trimmed = base.trim_end_matches('/');
    let relative = if base_trimmed.is_empty() || base_trimmed == "/" {
        destination_path.as_str()
    } else {
        match destination_path.strip_prefix(base_trimmed) {
            Some(rest) => rest,
            None => return bad_request("destination is outside the base path"),
        }
    };
    let dest_segments = match normalize_path(relative) {
        Ok(segments) => segments,
        Err(_) => return bad_request("invalid destination"),
    };
    if dest_segments.first().map(String::as_str) != Some(user.as_str()) {
        return forbidden();
    }
    match (segments, dest_segments.as_slice()) {
        ([from_user, from_slug, from_href], [to_user, to_slug, to_href])
            if from_user == user.as_str() && to_user == user.as_str() =>
        {
            let from = match CollectionPath::parse(from_user, from_slug) {
                Ok(path) => path,
                Err(_) => return not_found(),
            };
            let to = match CollectionPath::parse(to_user, to_slug) {
                Ok(path) => path,
                Err(_) => return not_found(),
            };
            let from_meta = match state.store.collection(&from) {
                Ok(meta) => meta,
                Err(_) => return not_found(),
            };
            let to_meta = match state.store.collection(&to) {
                Ok(meta) => meta,
                Err(_) => return not_found(),
            };
            if from_meta.is_read_only() || to_meta.is_read_only() {
                return forbidden();
            }
            let from_href = match Href::parse(from_href.clone()) {
                Ok(href) => href,
                Err(_) => return not_found(),
            };
            let to_href = match Href::parse(to_href.clone()) {
                Ok(href) => href,
                Err(_) => return bad_request("invalid destination href"),
            };
            match state
                .store
                .move_item(&from, &from_href, &to, &to_href, overwrite)
                .await
            {
                Ok(()) => Response::builder()
                    .status(StatusCode::CREATED)
                    .body(Body::empty())
                    .unwrap(),
                Err(error) => storage_error(error),
            }
        }
        ([from_user, from_slug], [to_user, to_slug])
            if from_user == user.as_str() && to_user == user.as_str() =>
        {
            let from = match CollectionPath::parse(from_user, from_slug) {
                Ok(path) => path,
                Err(_) => return not_found(),
            };
            let to = match CollectionPath::parse(to_user, to_slug) {
                Ok(path) => path,
                Err(_) => return bad_request("invalid destination"),
            };
            let from_meta = match state.store.collection(&from) {
                Ok(meta) => meta,
                Err(_) => return not_found(),
            };
            if from_meta.subscription.is_some() {
                return forbidden();
            }
            match state.store.move_collection(&from, &to, overwrite).await {
                Ok(_) => Response::builder()
                    .status(StatusCode::CREATED)
                    .body(Body::empty())
                    .unwrap(),
                Err(error) => storage_error(error),
            }
        }
        _ => method_not_allowed(allow_for(segments)),
    }
}

// ---------------------------------------------------------------------------
// REPORT
// ---------------------------------------------------------------------------

async fn handle_report(
    state: &Arc<AppState>,
    user: &UserName,
    segments: &[String],
    _headers: &HeaderMap,
    body: &[u8],
) -> Response {
    let resource = match resolve(state, user, segments) {
        Ok(resource) => resource,
        Err(response) => return response,
    };
    let (current, path, meta, permissions) = match &resource {
        Resource::Collection {
            current,
            path,
            meta,
            permissions,
        } => (current.clone(), path.clone(), meta.clone(), *permissions),
        _ => return method_not_allowed(allow_for(segments)),
    };
    let root = match xml::parse_xml(body, XML_LIMIT, XML_DEPTH) {
        Ok(root) => root,
        Err(error) => return xml_error(error),
    };
    let report = root.local.as_str();
    match report {
        "calendar-query" => {
            if meta.kind != CollectionKind::Calendar {
                return method_not_allowed(allow_for(segments));
            }
            let filter = match root.find("filter") {
                Some(filter) => match calendar_filter_from_xml(filter) {
                    Ok(filter) => filter,
                    Err(response) => return response,
                },
                None => CalendarFilter::match_all(),
            };
            if report_requests(&root, CALDAV, "expand") {
                return dav_error(StatusCode::FORBIDDEN, CALDAV, "supported-calendar-data");
            }
            let requested = report_props(&root);
            let items = match state.store.list_items(&path) {
                Ok(items) => items,
                Err(error) => return storage_error(error),
            };
            let mut out = response_start();
            for item in items {
                let calendar = match oxicale_ical::parse_calendar(
                    &item.payload,
                    IcalLimits {
                        max_bytes: state.store.options().max_calendar_item_size,
                        ..IcalLimits::default()
                    },
                ) {
                    Ok(calendar) => calendar,
                    Err(_) => continue,
                };
                match filter.matches(&calendar, state.store.options().default_timezone) {
                    oxicale_ical::FilterResult::Match => {}
                    _ => continue,
                }
                let resource = Resource::Item {
                    current: current.clone(),
                    path: path.clone(),
                    item: item.clone(),
                    permissions,
                };
                let props = requested.clone().unwrap_or_else(|| {
                    vec![
                        QName {
                            namespace: DAV.into(),
                            local: "getetag".into(),
                        },
                        QName {
                            namespace: CALDAV.into(),
                            local: "calendar-data".into(),
                        },
                    ]
                });
                let (found, missing) =
                    item_props(state, &resource, &props, CALDAV, "calendar-data");
                out.push_str(&response_for_resource(
                    &resource_href(state, &resource),
                    &found,
                    &missing,
                ));
            }
            out.push_str("</D:multistatus>");
            xml_response(StatusCode::MULTI_STATUS, out)
        }
        "calendar-multiget" => {
            if meta.kind != CollectionKind::Calendar {
                return method_not_allowed(allow_for(segments));
            }
            calendar_multiget(state, &current, &path, &root, permissions, false).await
        }
        "addressbook-query" => {
            if meta.kind != CollectionKind::AddressBook {
                return method_not_allowed(allow_for(segments));
            }
            let filter = match root.find("filter") {
                Some(filter) => match vcard_filter_from_xml(filter) {
                    Ok(filter) => filter,
                    Err(response) => return response,
                },
                None => VcardFilter::match_all(),
            };
            let requested = report_props(&root);
            let items = match state.store.list_items(&path) {
                Ok(items) => items,
                Err(error) => return storage_error(error),
            };
            let mut out = response_start();
            for item in items {
                let card = match oxicale_vcard::parse_vcard(
                    &item.payload,
                    VcardLimits {
                        max_bytes: state.store.options().max_contact_item_size,
                        ..VcardLimits::default()
                    },
                ) {
                    Ok(card) => card,
                    Err(_) => continue,
                };
                if !filter.matches(&card) {
                    continue;
                }
                let resource = Resource::Item {
                    current: current.clone(),
                    path: path.clone(),
                    item: item.clone(),
                    permissions,
                };
                let props = requested.clone().unwrap_or_else(|| {
                    vec![
                        QName {
                            namespace: DAV.into(),
                            local: "getetag".into(),
                        },
                        QName {
                            namespace: CARDDAV.into(),
                            local: "address-data".into(),
                        },
                    ]
                });
                let (found, missing) =
                    item_props(state, &resource, &props, CARDDAV, "address-data");
                out.push_str(&response_for_resource(
                    &resource_href(state, &resource),
                    &found,
                    &missing,
                ));
            }
            out.push_str("</D:multistatus>");
            xml_response(StatusCode::MULTI_STATUS, out)
        }
        "addressbook-multiget" => {
            if meta.kind != CollectionKind::AddressBook {
                return method_not_allowed(allow_for(segments));
            }
            calendar_multiget(state, &current, &path, &root, permissions, true).await
        }
        "free-busy-query" => {
            if meta.kind != CollectionKind::Calendar {
                return method_not_allowed(allow_for(segments));
            }
            let range = match root.find("time-range") {
                Some(element) => match time_range_from_xml(element) {
                    Ok(range) => range,
                    Err(response) => return response,
                },
                None => IcalTimeRange::unbounded(),
            };
            let items = match state.store.list_items(&path) {
                Ok(items) => items,
                Err(error) => return storage_error(error),
            };
            let mut periods = Vec::new();
            for item in items {
                if let Ok(calendar) = oxicale_ical::parse_calendar(
                    &item.payload,
                    IcalLimits {
                        max_bytes: state.store.options().max_calendar_item_size,
                        ..IcalLimits::default()
                    },
                ) {
                    if let Ok(mut found) = oxicale_ical::event_periods(
                        &calendar,
                        &range,
                        state.store.options().default_timezone,
                    ) {
                        periods.append(&mut found);
                    }
                }
            }
            let mut body = String::from(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Oxicale//EN\r\nBEGIN:VFREEBUSY\r\n",
            );
            body.push_str(&format!(
                "DTSTAMP:{}\r\n",
                Utc::now().format("%Y%m%dT%H%M%SZ")
            ));
            if let Some(start) = range.start {
                body.push_str(&format!("DTSTART:{}\r\n", start.format("%Y%m%dT%H%M%SZ")));
            }
            if let Some(end) = range.end {
                body.push_str(&format!("DTEND:{}\r\n", end.format("%Y%m%dT%H%M%SZ")));
            }
            let mut merged: Vec<(DateTime<Utc>, DateTime<Utc>)> = Vec::new();
            for (start, end) in periods {
                if let Some((_, last_end)) = merged.last_mut() {
                    if start <= *last_end {
                        if end > *last_end {
                            *last_end = end;
                        }
                        continue;
                    }
                }
                merged.push((start, end));
            }
            for (start, end) in merged {
                body.push_str(&format!(
                    "FREEBUSY;FBTYPE=BUSY:{}/{}\r\n",
                    start.format("%Y%m%dT%H%M%SZ"),
                    end.format("%Y%m%dT%H%M%SZ")
                ));
            }
            body.push_str("END:VFREEBUSY\r\nEND:VCALENDAR\r\n");
            Response::builder()
                .status(StatusCode::OK)
                .header(
                    axum::http::header::CONTENT_TYPE,
                    "text/calendar; charset=utf-8",
                )
                .body(Body::from(body))
                .unwrap()
        }
        "sync-collection" => sync_collection(state, &current, &path, &root, permissions).await,
        _ => simple_response(StatusCode::FORBIDDEN, "text/plain", "unsupported report"),
    }
}

fn report_props(root: &Element) -> Option<Vec<QName>> {
    root.child(DAV, "prop").map(|prop| {
        prop.children
            .iter()
            .map(|child| QName {
                namespace: child.namespace.clone(),
                local: child.local.clone(),
            })
            .collect()
    })
}

fn item_props(
    state: &AppState,
    resource: &Resource,
    requested: &[QName],
    data_namespace: &str,
    data_local: &str,
) -> (String, Vec<QName>) {
    let mut found = String::new();
    let mut missing = Vec::new();
    for qname in requested {
        if qname.namespace == data_namespace && qname.local == data_local {
            if let Resource::Item { item, .. } = resource {
                found.push_str(&format!(
                    "<{}:{data_local}>{}</{}:{data_local}>",
                    ns_prefix(data_namespace),
                    xml::escape(&item.payload),
                    ns_prefix(data_namespace)
                ));
            } else {
                missing.push(qname.clone());
            }
            continue;
        }
        match property_inner(state, resource, qname) {
            Some(value) => {
                found.push_str(&format!(
                    "<{tag}>{value}</{tag}>",
                    tag = tag(&qname.namespace, &qname.local)
                ));
            }
            None => missing.push(qname.clone()),
        }
    }
    (found, missing)
}

fn report_requests(root: &Element, namespace: &str, local: &str) -> bool {
    fn walk(element: &Element, namespace: &str, local: &str) -> bool {
        if element.namespace == namespace && element.local == local {
            return true;
        }
        element
            .children
            .iter()
            .any(|child| walk(child, namespace, local))
    }
    walk(root, namespace, local)
}

async fn calendar_multiget(
    state: &Arc<AppState>,
    current: &UserName,
    path: &CollectionPath,
    root: &Element,
    permissions: Permissions,
    vcard: bool,
) -> Response {
    let mut seen = BTreeSet::new();
    let mut hrefs = Vec::new();
    for element in root.children_named(DAV, "href") {
        let raw = element.text_trimmed();
        let parsed = match url::Url::parse(raw) {
            Ok(url) => url.path().to_string(),
            Err(_) => raw.to_string(),
        };
        let base = state.config.server.normalized_base_path();
        let base_trimmed = base.trim_end_matches('/');
        let relative = if base_trimmed.is_empty() || base_trimmed == "/" {
            parsed.as_str()
        } else {
            parsed.strip_prefix(base_trimmed).unwrap_or(&parsed)
        };
        let segments = match normalize_path(relative) {
            Ok(segments) => segments,
            Err(_) => continue,
        };
        if segments.len() != 3
            || segments[0] != path.user.as_str()
            || segments[1] != path.slug.as_str()
        {
            continue;
        }
        if seen.insert(segments[2].clone()) {
            hrefs.push(segments[2].clone());
        }
    }
    let requested = report_props(root);
    let data_namespace = if vcard { CARDDAV } else { CALDAV };
    let data_local = if vcard {
        "address-data"
    } else {
        "calendar-data"
    };
    let mut out = response_start();
    for href in hrefs {
        let href = match Href::parse(href) {
            Ok(href) => href,
            Err(_) => continue,
        };
        match state.store.get_item(path, &href) {
            Ok(item) => {
                let resource = Resource::Item {
                    current: current.clone(),
                    path: path.clone(),
                    item: item.clone(),
                    permissions,
                };
                let props = requested.clone().unwrap_or_else(|| {
                    vec![
                        QName {
                            namespace: DAV.into(),
                            local: "getetag".into(),
                        },
                        QName {
                            namespace: data_namespace.into(),
                            local: data_local.into(),
                        },
                    ]
                });
                let (found, missing) =
                    item_props(state, &resource, &props, data_namespace, data_local);
                out.push_str(&response_for_resource(
                    &resource_href(state, &resource),
                    &found,
                    &missing,
                ));
            }
            Err(_) => {
                out.push_str(&format!(
                    "<D:response><D:href>{}</D:href><D:status>HTTP/1.1 404 Not Found</D:status></D:response>",
                    xml::escape(&href_for(
                        state,
                        &[
                            path.user.as_str().into(),
                            path.slug.as_str().into(),
                            href.as_str().into()
                        ],
                        false
                    ))
                ));
            }
        }
    }
    out.push_str("</D:multistatus>");
    xml_response(StatusCode::MULTI_STATUS, out)
}

async fn sync_collection(
    state: &Arc<AppState>,
    current: &UserName,
    path: &CollectionPath,
    root: &Element,
    permissions: Permissions,
) -> Response {
    let token = root
        .child(DAV, "sync-token")
        .map(|element| element.text_trimmed().to_string())
        .unwrap_or_default();
    let sync_level = root
        .child(DAV, "sync-level")
        .map(|element| element.text_trimmed().to_string());
    if sync_level.as_deref().is_some_and(|level| level != "1") {
        return bad_request("only sync-level 1 is supported");
    }
    let requested = report_props(root);
    let (data_namespace, data_local) = match state.store.collection(path) {
        Ok(meta) if meta.kind == CollectionKind::AddressBook => (CARDDAV, "address-data"),
        _ => (CALDAV, "calendar-data"),
    };
    let mut out = response_start();
    let new_token;
    if token.is_empty() {
        let items = match state.store.list_items(path) {
            Ok(items) => items,
            Err(error) => return storage_error(error),
        };
        new_token = match state.store.sync_token(path) {
            Ok(token) => token,
            Err(error) => return storage_error(error),
        };
        for item in items {
            let resource = Resource::Item {
                current: current.clone(),
                path: path.clone(),
                item,
                permissions,
            };
            let props = requested.clone().unwrap_or_else(|| {
                vec![QName {
                    namespace: DAV.into(),
                    local: "getetag".into(),
                }]
            });
            let (found, missing) = item_props(state, &resource, &props, data_namespace, data_local);
            out.push_str(&response_for_resource(
                &resource_href(state, &resource),
                &found,
                &missing,
            ));
        }
    } else {
        let changes = match state.store.changes_since(path, &token) {
            Ok(changes) => changes,
            Err(_) => {
                return dav_error(StatusCode::FORBIDDEN, DAV, "valid-sync-token");
            }
        };
        new_token = changes.token;
        for change in changes.changes {
            let href = href_for(
                state,
                &[
                    path.user.as_str().into(),
                    path.slug.as_str().into(),
                    change.href.clone(),
                ],
                false,
            );
            if change.operation == ChangeOperation::Delete {
                out.push_str(&format!(
                    "<D:response><D:href>{}</D:href><D:status>HTTP/1.1 404 Not Found</D:status></D:response>",
                    xml::escape(&href)
                ));
            } else {
                let parsed = Href::parse(change.href.clone()).ok();
                if let Some(item) = parsed
                    .as_ref()
                    .and_then(|href| state.store.get_item(path, href).ok())
                {
                    let resource = Resource::Item {
                        current: current.clone(),
                        path: path.clone(),
                        item,
                        permissions,
                    };
                    let props = requested.clone().unwrap_or_else(|| {
                        vec![QName {
                            namespace: DAV.into(),
                            local: "getetag".into(),
                        }]
                    });
                    let (found, missing) =
                        item_props(state, &resource, &props, data_namespace, data_local);
                    out.push_str(&response_for_resource(
                        &resource_href(state, &resource),
                        &found,
                        &missing,
                    ));
                } else {
                    let etag = change.etag.unwrap_or_default();
                    out.push_str(&format!(
                        "<D:response><D:href>{}</D:href><D:propstat><D:prop><D:getetag>{}</D:getetag></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>",
                        xml::escape(&href),
                        xml::escape(&format!("\"{etag}\""))
                    ));
                }
            }
        }
    }
    out.push_str(&format!(
        "<D:sync-token>{}</D:sync-token></D:multistatus>",
        xml::escape(&new_token)
    ));
    xml_response(StatusCode::MULTI_STATUS, out)
}

fn calendar_filter_from_xml(filter: &Element) -> Result<CalendarFilter, Response> {
    let comp = filter
        .child(CALDAV, "comp-filter")
        .ok_or_else(|| bad_request("missing comp-filter"))?;
    Ok(CalendarFilter {
        root: component_filter_from_xml(comp)?,
    })
}

fn component_filter_from_xml(element: &Element) -> Result<ComponentFilter, Response> {
    let mut filter = ComponentFilter {
        name: element.attr("name").map(str::to_string),
        is_not_defined: element.child(CALDAV, "is-not-defined").is_some(),
        time_range: element
            .child(CALDAV, "time-range")
            .map(time_range_from_xml)
            .transpose()?,
        prop_filters: Vec::new(),
        comp_filters: Vec::new(),
    };
    for child in &element.children {
        match (child.namespace.as_str(), child.local.as_str()) {
            (CALDAV, "comp-filter") => {
                filter.comp_filters.push(component_filter_from_xml(child)?);
            }
            (CALDAV, "prop-filter") => {
                let mut prop = PropFilter {
                    name: child.attr("name").unwrap_or_default().to_string(),
                    is_not_defined: child.child(CALDAV, "is-not-defined").is_some(),
                    text_matches: Vec::new(),
                    param_filters: Vec::new(),
                };
                for item in &child.children {
                    match (item.namespace.as_str(), item.local.as_str()) {
                        (CALDAV, "text-match") => prop.text_matches.push(ical_text_match(item)?),
                        (CALDAV, "param-filter") => {
                            let mut param = ParamFilter {
                                name: item.attr("name").unwrap_or_default().to_string(),
                                is_not_defined: item.child(CALDAV, "is-not-defined").is_some(),
                                text_matches: Vec::new(),
                            };
                            for text in item.children_named(CALDAV, "text-match") {
                                param.text_matches.push(ical_text_match(text)?);
                            }
                            prop.param_filters.push(param);
                        }
                        _ => {}
                    }
                }
                filter.prop_filters.push(prop);
            }
            _ => {}
        }
    }
    Ok(filter)
}

fn ical_text_match(element: &Element) -> Result<IcalTextMatch, Response> {
    let collation = element.attr("collation").unwrap_or("i;ascii-casemap");
    let case_sensitive = collation == "i;octet";
    let kind = match element.attr("match-type").unwrap_or("contains") {
        "equals" => IcalTextMatchKind::Equals,
        "contains" => IcalTextMatchKind::Contains,
        "starts-with" => IcalTextMatchKind::StartsWith,
        "ends-with" => IcalTextMatchKind::EndsWith,
        _ => return Err(bad_request("unsupported match-type")),
    };
    Ok(IcalTextMatch {
        value: element.text.to_string(),
        kind,
        negate: element.attr("negate-condition") == Some("yes"),
        case_sensitive,
    })
}

fn vcard_filter_from_xml(filter: &Element) -> Result<VcardFilter, Response> {
    let test = match filter.attr("test").unwrap_or("anyof") {
        "allof" => MatchTest::AllOf,
        _ => MatchTest::AnyOf,
    };
    let mut result = VcardFilter {
        test,
        prop_filters: Vec::new(),
    };
    for child in filter.children_named(CARDDAV, "prop-filter") {
        let mut prop = VcardPropFilter {
            name: child.attr("name").unwrap_or_default().to_string(),
            is_not_defined: child.child(CARDDAV, "is-not-defined").is_some(),
            text_matches: Vec::new(),
            param_filters: Vec::new(),
        };
        for item in &child.children {
            match (item.namespace.as_str(), item.local.as_str()) {
                (CARDDAV, "text-match") => prop.text_matches.push(vcard_text_match(item)?),
                (CARDDAV, "param-filter") => {
                    let mut param = VcardParamFilter {
                        name: item.attr("name").unwrap_or_default().to_string(),
                        is_not_defined: item.child(CARDDAV, "is-not-defined").is_some(),
                        text_matches: Vec::new(),
                    };
                    for text in item.children_named(CARDDAV, "text-match") {
                        param.text_matches.push(vcard_text_match(text)?);
                    }
                    prop.param_filters.push(param);
                }
                _ => {}
            }
        }
        result.prop_filters.push(prop);
    }
    Ok(result)
}

fn vcard_text_match(text: &Element) -> Result<VcardTextMatch, Response> {
    let collation = text.attr("collation").unwrap_or("i;ascii-casemap");
    let kind = match text.attr("match-type").unwrap_or("contains") {
        "equals" => VcardTextMatchKind::Equals,
        "contains" => VcardTextMatchKind::Contains,
        "starts-with" => VcardTextMatchKind::StartsWith,
        "ends-with" => VcardTextMatchKind::EndsWith,
        _ => return Err(bad_request("unsupported match-type")),
    };
    Ok(VcardTextMatch {
        value: text.text.to_string(),
        kind,
        negate: text.attr("negate-condition") == Some("yes"),
        case_sensitive: collation == "i;octet",
    })
}

fn time_range_from_xml(element: &Element) -> Result<IcalTimeRange, Response> {
    Ok(IcalTimeRange {
        start: element
            .attr("start")
            .map(parse_ical_utc)
            .transpose()?
            .flatten(),
        end: element
            .attr("end")
            .map(parse_ical_utc)
            .transpose()?
            .flatten(),
    })
}

fn parse_ical_utc(value: &str) -> Result<Option<DateTime<Utc>>, Response> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() == 8 {
        let date = NaiveDate::parse_from_str(value, "%Y%m%d")
            .map_err(|_| bad_request("invalid time-range date"))?;
        let naive = date
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| bad_request("invalid time-range date"))?;
        return Ok(Some(Utc.from_utc_datetime(&naive)));
    }
    let naive = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%SZ")
        .or_else(|_| NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S"))
        .map_err(|_| bad_request("invalid time-range datetime"))?;
    Ok(Some(Utc.from_utc_datetime(&naive)))
}
