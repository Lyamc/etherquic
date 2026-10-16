//! Open connections in the window. Switching the current one leaves the
//! others running. Closing one leaves the rest.

/// One connection the window is keeping.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionTab {
    pub id: u64,
    pub label: String,
}

/// The list of connections and which one is current.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectionTabs {
    tabs: Vec<ConnectionTab>,
    active: Option<u64>,
    next: u64,
}

impl Default for ConnectionTabs {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionTabs {
    pub fn new() -> Self {
        Self {
            tabs: Vec::new(),
            active: None,
            next: 1,
        }
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &ConnectionTab> {
        self.tabs.iter()
    }

    pub fn active_id(&self) -> Option<u64> {
        self.active
    }

    pub fn active(&self) -> Option<&ConnectionTab> {
        let id = self.active?;
        self.tabs.iter().find(|tab| tab.id == id)
    }

    pub fn is_open(&self, id: u64) -> bool {
        self.tabs.iter().any(|tab| tab.id == id)
    }

    /// Open a connection and make it current. The others stay open.
    pub fn open(&mut self, label: impl Into<String>) -> u64 {
        let id = self.next;
        self.next += 1;
        self.tabs.push(ConnectionTab {
            id,
            label: label.into(),
        });
        self.active = Some(id);
        id
    }

    /// Make `id` current. Returns false when that connection is not open.
    /// The others stay open.
    pub fn switch(&mut self, id: u64) -> bool {
        if !self.is_open(id) {
            return false;
        }
        self.active = Some(id);
        true
    }

    /// Close one connection. When it was current, another open one becomes
    /// current. Returns false when `id` was not open.
    pub fn close(&mut self, id: u64) -> bool {
        let Some(pos) = self.tabs.iter().position(|tab| tab.id == id) else {
            return false;
        };
        let was_active = self.active == Some(id);
        self.tabs.remove(pos);
        if was_active {
            self.active = self
                .tabs
                .get(pos)
                .or_else(|| self.tabs.last())
                .map(|tab| tab.id);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_switch_and_close_leaves_the_other_connection_current() {
        let mut tabs = ConnectionTabs::new();
        assert!(tabs.active().is_none());
        assert!(tabs.is_empty());

        let first = tabs.open("one");
        let second = tabs.open("two");
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs.active_id(), Some(second));
        assert!(tabs.is_open(first));
        assert!(tabs.is_open(second));

        assert!(tabs.switch(first));
        assert_eq!(tabs.active_id(), Some(first));
        assert_eq!(tabs.len(), 2);
        assert!(tabs.is_open(second));

        assert!(tabs.close(first));
        assert_eq!(tabs.len(), 1);
        assert!(!tabs.is_open(first));
        assert!(tabs.is_open(second));
        assert_eq!(tabs.active_id(), Some(second));
        assert_eq!(tabs.active().map(|tab| tab.label.as_str()), Some("two"));
        assert!(!tabs.switch(first));
    }
}
