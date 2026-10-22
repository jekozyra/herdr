use super::*;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileUploadNotice {
    text: String,
    active: bool,
}

impl ClientShellState {
    pub(crate) fn file_upload_pane(&self) -> Option<String> {
        match self.clipboard_image_target()? {
            crate::protocol::ClientClipboardImageTarget::Pane(id) => Some(id),
            _ => None,
        }
    }

    pub(crate) fn set_file_upload_notice(&mut self, text: Option<String>, active: bool) -> bool {
        let text = text.map(|text| FileUploadNotice { text, active });
        if self.file_upload_notice == text {
            return false;
        }
        self.file_upload_notice = text;
        true
    }

    pub(super) fn render_file_upload_notice(
        &self,
        frame: &mut FrameData,
        cols: u16,
        rows: u16,
    ) -> Option<Rect> {
        let notice = self.file_upload_notice.as_ref()?;
        let area = crate::ui::centered_popup_rect(Rect::new(0, 0, cols, rows), 76, 12)?;
        let mut buffer = frame.to_ratatui_buffer()?;
        let palette = &self.config.palette;
        // One client-owned floating card; geometry and I/O remain independent of panes.
        Clear.render(area, &mut buffer);
        let block = Block::default()
            .title(" File upload ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette.accent))
            .style(Style::default().bg(palette.panel_bg));
        let inner = block.inner(area);
        block.render(area, &mut buffer);
        let content = Rect::new(
            inner.x.saturating_add(1),
            inner.y.saturating_add(1),
            inner.width.saturating_sub(2),
            inner.height.saturating_sub(3),
        );
        Paragraph::new(notice.text.replace(" · ", "\n"))
            .style(Style::default().fg(palette.text).bg(palette.panel_bg))
            .wrap(Wrap { trim: false })
            .render(content, &mut buffer);
        let footer = Rect::new(
            content.x,
            inner.bottom().saturating_sub(1),
            content.width,
            1,
        );
        Paragraph::new(if notice.active {
            "Esc  cancel upload"
        } else {
            "Esc  close"
        })
        .style(Style::default().fg(palette.overlay0).bg(palette.panel_bg))
        .render(footer, &mut buffer);
        let cursor = frame
            .cursor
            .clone()
            .filter(|cursor| !area.contains((cursor.x, cursor.y).into()));
        frame.replace_from_ratatui_buffer_preserving_effects(&buffer, cursor);
        Some(area)
    }
}

#[cfg(test)]
impl ClientShellState {
    pub(crate) fn file_upload_test_shell() -> Self {
        let mut shell = Self::new(ClientShellConfig::from_config(
            &crate::config::Config::default(),
        ));
        shell.set_snapshot(Box::new(super::tests::snapshot()));
        shell
    }
}
