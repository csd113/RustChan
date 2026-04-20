use super::{escape_html, render_board_settings_card, AdminPanelViewModel};

pub(super) fn render(view: &AdminPanelViewModel<'_>) -> String {
    let mut board_cards = String::new();
    for (index, board) in view.boards.iter().enumerate() {
        let board_assets = view
            .appearance
            .board_banners
            .iter()
            .filter(|asset| {
                asset.scope == crate::models::BannerScope::Board && asset.board_id == Some(board.id)
            })
            .cloned()
            .collect::<Vec<_>>();
        board_cards.push_str(&render_board_settings_card(
            board,
            index,
            view.boards,
            view.csrf_token,
            view.appearance.themes,
            &board_assets,
            view.open_section,
        ));
    }

    render_admin_boards_section(view.csrf_token, &board_cards)
}

fn render_admin_boards_section(csrf_token: &str, board_cards: &str) -> String {
    format!(
        r#"<div class="admin-panel-boards" id="boards">
<!-- ═══════════════════════════════════════════════════════════════════════════
     // boards
     ═══════════════════════════════════════════════════════════════════════════ -->
<section class="admin-section">
<h2>// boards</h2>
<div class="admin-subsection">
  <div class="admin-card-header">
    <h3>// board directory</h3>
  <p>Open a board to edit its settings.</p>
  </div>
  <p class="admin-order-note">Board order is shared across the homepage, top bar, and this panel. SFW and NSFW boards each keep their own order.</p>
  <div class="admin-board-cards">{board_cards}</div>
</div>
<div class="admin-subsection">
  <div class="admin-card-header">
    <h3>// create board</h3>
    <p>Start with the short name and label, then edit the rest in its board card above.</p>
  </div>
  <form method="POST" action="/admin/board/create" class="admin-board-create-form admin-quick-form">
  <input type="hidden" name="_csrf" value="{csrf}">
  <label class="admin-quick-field">Short name
    <input type="text" name="short_name" maxlength="8" required placeholder="tech">
  </label>
  <label class="admin-quick-field">Display name
    <input type="text" name="name" maxlength="64" required placeholder="Technology">
  </label>
  <label class="admin-quick-field">Description
    <input type="text" name="description" maxlength="256" placeholder="Programming, hardware, and internet culture">
  </label>
  <label class="admin-inline-checkbox admin-quick-checkbox"><input type="checkbox" name="nsfw" value="1"> NSFW board</label>
  <label class="admin-inline-checkbox admin-quick-checkbox"><input type="checkbox" name="allow_audio" value="1"> Enable audio uploads</label>
  <button type="submit">create</button>
  </form>
</div>
</section>
</div>"#,
        board_cards = board_cards,
        csrf = escape_html(csrf_token),
    )
}
