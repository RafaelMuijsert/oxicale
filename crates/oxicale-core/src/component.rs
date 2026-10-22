use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum CalendarComponent {
    VEvent,
    VTodo,
    VJournal,
}

impl CalendarComponent {
    pub const ALL: [Self; 3] = [Self::VEvent, Self::VTodo, Self::VJournal];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::VEvent => "VEVENT",
            Self::VTodo => "VTODO",
            Self::VJournal => "VJOURNAL",
        }
    }
}

impl FromStr for CalendarComponent {
    type Err = CoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_uppercase().as_str() {
            "VEVENT" => Ok(Self::VEvent),
            "VTODO" => Ok(Self::VTodo),
            "VJOURNAL" => Ok(Self::VJournal),
            other => Err(CoreError::InvalidComponent(other.to_string())),
        }
    }
}

impl fmt::Display for CalendarComponent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ItemComponent {
    VEvent,
    VTodo,
    VJournal,
    VCard,
}

impl ItemComponent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VEvent => "VEVENT",
            Self::VTodo => "VTODO",
            Self::VJournal => "VJOURNAL",
            Self::VCard => "VCARD",
        }
    }

    pub fn calendar(self) -> Option<CalendarComponent> {
        match self {
            Self::VEvent => Some(CalendarComponent::VEvent),
            Self::VTodo => Some(CalendarComponent::VTodo),
            Self::VJournal => Some(CalendarComponent::VJournal),
            Self::VCard => None,
        }
    }
}

impl fmt::Display for ItemComponent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<CalendarComponent> for ItemComponent {
    fn from(value: CalendarComponent) -> Self {
        match value {
            CalendarComponent::VEvent => Self::VEvent,
            CalendarComponent::VTodo => Self::VTodo,
            CalendarComponent::VJournal => Self::VJournal,
        }
    }
}

/// A non-empty subset of calendar component types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct ComponentSet(u8);

impl ComponentSet {
    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn all() -> Self {
        Self(0b111)
    }

    pub const fn one(component: CalendarComponent) -> Self {
        Self(match component {
            CalendarComponent::VEvent => 0b001,
            CalendarComponent::VTodo => 0b010,
            CalendarComponent::VJournal => 0b100,
        })
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn contains(self, component: CalendarComponent) -> bool {
        self.0 & Self::one(component).0 != 0
    }

    pub fn insert(&mut self, component: CalendarComponent) {
        self.0 |= Self::one(component).0;
    }

    pub fn remove(&mut self, component: CalendarComponent) {
        self.0 &= !Self::one(component).0;
    }

    pub fn iter(self) -> impl Iterator<Item = CalendarComponent> {
        CalendarComponent::ALL
            .into_iter()
            .filter(move |c| self.contains(*c))
    }

    pub fn to_vec(self) -> Vec<CalendarComponent> {
        self.iter().collect()
    }

    pub fn parse_all(values: &[String]) -> CoreResult<Self> {
        let mut set = Self::empty();
        for value in values {
            set.insert(value.parse()?);
        }
        Ok(set)
    }

    pub fn from_set(values: BTreeSet<CalendarComponent>) -> Self {
        let mut set = Self::empty();
        for value in values {
            set.insert(value);
        }
        set
    }

    pub fn as_strings(self) -> Vec<String> {
        self.iter().map(|c| c.as_str().to_string()).collect()
    }
}

impl Serialize for ComponentSet {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.as_strings().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ComponentSet {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let values = Vec::<CalendarComponent>::deserialize(deserializer)?;
        let mut set = Self::empty();
        for value in values {
            set.insert(value);
        }
        Ok(set)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollectionKind {
    Calendar,
    AddressBook,
}

impl CollectionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Calendar => "calendar",
            Self::AddressBook => "addressbook",
        }
    }
}

impl fmt::Display for CollectionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_parsing() {
        assert_eq!(
            "VTODO".parse::<CalendarComponent>().unwrap(),
            CalendarComponent::VTodo
        );
        assert_eq!(
            "vevent".parse::<CalendarComponent>().unwrap(),
            CalendarComponent::VEvent
        );
        assert!("VFREEBUSY".parse::<CalendarComponent>().is_err());
    }

    #[test]
    fn component_sets() {
        assert!(ComponentSet::all().contains(CalendarComponent::VJournal));
        assert_eq!(ComponentSet::all().iter().count(), 3);
        let mut set = ComponentSet::empty();
        set.insert(CalendarComponent::VTodo);
        assert!(!set.is_empty());
        assert!(set.contains(CalendarComponent::VTodo));
        assert!(!set.contains(CalendarComponent::VEvent));
        set.remove(CalendarComponent::VTodo);
        assert!(set.is_empty());
    }

    #[test]
    fn component_set_serde() {
        let json = serde_json::to_string(&ComponentSet::all()).unwrap();
        assert_eq!(json, "[\"VEVENT\",\"VTODO\",\"VJOURNAL\"]");
        let parsed: ComponentSet = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, ComponentSet::all());
    }
}
