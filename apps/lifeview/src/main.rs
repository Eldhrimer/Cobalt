use kobo_sdk::prelude::*;
use std::process::ExitCode;

/// The bottom-bar destinations: (action name, label).
const TABS: [(&str, &str); 5] = [
    ("weeks", "Weeks"),
    ("today", "Today"),
    ("years", "Years"),
    ("eras", "Eras"),
    ("settings", "Settings"),
];

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Tab {
    #[default]
    Weeks,
    Today,
    Years,
    Eras,
    Settings,
}

impl Tab {
    fn index(self) -> usize {
        match self {
            Self::Weeks => 0,
            Self::Today => 1,
            Self::Years => 2,
            Self::Eras => 3,
            Self::Settings => 4,
        }
    }

    fn title(self) -> &'static str {
        TABS[self.index()].1
    }

    fn from_action(action: ActionId) -> Option<Self> {
        [Self::Weeks, Self::Today, Self::Years, Self::Eras, Self::Settings]
            .into_iter()
            .find(|tab| action == action_id(TABS[tab.index()].0))
    }
}

#[derive(Default)]
struct Lifeview {
    tab: Tab,
}

impl Lifeview {
    fn show(&self, context: &mut Context) {
        context.set_screen(
            ScreenBuilder::new(TABS[self.tab.index()].0)
                .heading(self.tab.title())
                .text("Coming soon.")
                .nav_bar(self.tab.index(), TABS)
                .build(),
        );
    }
}

impl KoboApp for Lifeview {
    fn on_start(&mut self, context: &mut Context) {
        self.show(context);
    }

    fn on_action(&mut self, context: &mut Context, action: ActionId) {
        if let Some(tab) = Tab::from_action(action) {
            self.tab = tab;
            self.show(context);
        }
    }
}

fn main() -> ExitCode {
    match kobo_sdk::run("lifeview", Lifeview::default()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("lifeview: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Lifeview, Tab};
    use kobo_sdk::prelude::*;
    use kobo_sdk::CLARA_BW_METRICS;

    fn screen_of(app: &mut Lifeview, context: &mut Context) -> Screen {
        app.show(context);
        context
            .take_commands()
            .into_iter()
            .find_map(|command| match command {
                Command::SetScreen(screen) => Some(screen),
                _ => None,
            })
            .expect("show sets a screen")
    }

    #[test]
    fn every_tab_fits_a_clara() {
        for tab in [Tab::Weeks, Tab::Today, Tab::Years, Tab::Eras, Tab::Settings] {
            let mut app = Lifeview { tab };
            let mut context = Context::default();
            let screen = screen_of(&mut app, &mut context);
            assert!(screen.validate(&CLARA_BW_METRICS).is_empty(), "{tab:?}");
        }
    }

    #[test]
    fn tapping_a_destination_switches_tab() {
        let mut app = Lifeview::default();
        let mut context = Context::default();
        app.on_action(&mut context, action_id("eras"));
        assert_eq!(app.tab, Tab::Eras);
    }
}
