use std::collections::VecDeque;
use std::sync::Arc;

use zeroize::Zeroizing;

use super::{Error5250, ErrorLine, Field, Input, ScreenSize, Tn5250Screen};

const MAX_IMAGES: usize = 8;
pub(super) const TOKEN_BYTES: usize = 16;
type Token = [u8; TOKEN_BYTES];

#[derive(Clone, Default)]
pub(super) struct SavedScreens {
    images: VecDeque<(Token, Arc<ScreenImage>)>,
}

struct ScreenImage {
    size: ScreenSize,
    cells: Zeroizing<Vec<u8>>,
    fields: Vec<Field>,
    cursor: usize,
    home: Option<usize>,
    error_row: usize,
    error_line: Option<ErrorLine>,
    keyboard_locked: bool,
    operator_error: bool,
    insert_mode: bool,
    pending: Option<Input>,
}

impl Tn5250Screen {
    pub(super) fn save_screen(&mut self) -> Result<Token, Error5250> {
        // IBM SC30-3533-02 leaves the saved data workstation-specific and
        // requires the host to echo it unchanged. Keep field data local.
        let mut token = [0; TOKEN_BYTES];
        getrandom::fill(&mut token).map_err(|_| Error5250::SaveUnavailable)?;
        if self.saved.images.iter().any(|(key, _)| key == &token) {
            return Err(Error5250::SaveUnavailable);
        }
        let image = ScreenImage {
            size: self.size,
            cells: self.cells.clone(),
            fields: self.fields.clone(),
            cursor: self.cursor,
            home: self.home,
            error_row: self.error_row,
            error_line: self.error_line.clone(),
            keyboard_locked: self.keyboard_locked,
            operator_error: self.operator_error,
            insert_mode: self.insert_mode,
            pending: self.pending,
        };
        if self.saved.images.len() == MAX_IMAGES {
            self.saved.images.pop_front();
        }
        self.saved.images.push_back((token, Arc::new(image)));
        Ok(token)
    }

    pub(super) fn restore_screen(&mut self, token: &Token) -> Result<(), Error5250> {
        let (_, image) = self
            .saved
            .images
            .iter()
            .find(|(key, _)| key == token)
            .ok_or(Error5250::Malformed("unknown or expired 5250 saved screen"))?;

        // Message waiting and Cancel Invite are transport state. They stay
        // live across display restores.
        self.size = image.size;
        self.cells.clone_from(&image.cells);
        self.fields.clone_from(&image.fields);
        self.cursor = image.cursor;
        self.home = image.home;
        self.error_row = image.error_row;
        self.error_line.clone_from(&image.error_line);
        self.keyboard_locked = image.keyboard_locked;
        self.operator_error = image.operator_error;
        self.insert_mode = image.insert_mode;
        self.pending = image.pending;
        Ok(())
    }
}
