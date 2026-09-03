use gtk4 as gtk;

pub const CSS: &str = r#"
    /* ── Kill every possible headerbar underline source ────────── */
    headerbar, headerbar:backdrop {
        box-shadow: none;
        border: none;
        border-bottom: none;
        border-bottom-width: 0;
        border-bottom-style: none;
        border-bottom-color: transparent;
    }
    .titlebar, .titlebar:backdrop {
        box-shadow: none;
        border-bottom: none;
        border-bottom-width: 0;
    }
    .titlebar separator,
    headerbar separator {
        min-height: 0;
        opacity: 0;
        background: transparent;
    }
    window.background > decoration {
        box-shadow: none;
    }

    /* ── Model cards & Preferences cards: single clean flat card ── */
    preferencesgroup,
    preferencesgroup box,
    box.boxed-list {
        background-color: transparent;
        border: none;
    }

    /* ── Model cards: flat with generous padding ───────────────── */
    .card,
    .card-active {
        background-color: alpha(@theme_fg_color, 0.06);
        border: 1px solid alpha(@theme_fg_color, 0.12);
        border-radius: 0px;
        padding: 14px 16px;
        margin: 3px 0px;
    }
    .card-active {
        background-color: alpha(@theme_fg_color, 0.08);
        border-left: 3px solid #2dd4f0;
    }

    /* ── Preferences cards ─────────────────────────────────────── */
    list.boxed-list {
        background-color: alpha(@theme_fg_color, 0.06);
        border: 1px solid alpha(@theme_fg_color, 0.12);
        border-radius: 0px;
    }

    /* ── Inner rows inside preferences cards ───────────────────── */
    list.boxed-list > row {
        border-radius: 0px;
        background-color: transparent;
    }

    list.boxed-list > row:not(:last-child) {
        border-bottom: 1px solid alpha(@theme_fg_color, 0.08);
    }

    /* ── All Buttons, Entries, Dropdowns & Steppers: 0px Sharp ──── */
    button,
    entry,
    dropdown,
    combobox,
    spinbutton,
    spinbutton button {
        border-radius: 0px;
    }

    /* ── Menubar & Popover Menu Items: 0px Sharp Hover & Dropdown ─ */
    menubar,
    menubar *,
    menubar item,
    menubar > item,
    menubar > item:hover,
    menubar > item:active,
    menubar > item:focus,
    menubar > item:selected,
    menubar.popover,
    menubar.popover *,
    menubar.popover item,
    menubar.popover > item,
    menubar.popover > item:hover,
    menubar.popover > item:active,
    menubar.popover > item:focus,
    menubar.popover > item:selected,
    headerbar menubar,
    headerbar menubar *,
    headerbar menubar item,
    headerbar menubar > item,
    headerbar menubar > item:hover,
    headerbar menubar > item:active,
    headerbar menubar > item:focus,
    headerbar menubar > item:selected,
    popovermenubar,
    popovermenubar *,
    popovermenubar menubaritem,
    popovermenubar > menubaritem,
    popovermenubar item,
    popovermenubar button,
    popovermenubar > item,
    popovermenubar > button,
    popovermenubar menubaritem:hover,
    popovermenubar menubaritem:active,
    popovermenubar menubaritem:focus,
    popovermenubar menubaritem:checked,
    popovermenubar menubaritem:selected,
    popovermenubar item:hover,
    popovermenubar item:active,
    popovermenubar item:focus,
    popovermenubar item:checked,
    popovermenubar item:selected,
    popovermenubar button:hover,
    popovermenubar button:active,
    popovermenubar button:focus,
    popovermenubar button:checked,
    headerbar menubaritem,
    headerbar menubaritem:hover,
    headerbar menubaritem:active,
    headerbar menubaritem:focus,
    headerbar menubaritem:checked,
    headerbar menubaritem:selected,
    headerbar popovermenubar,
    headerbar popovermenubar *,
    menubaritem,
    menubaritem:hover,
    menubaritem:active,
    menubaritem:focus,
    menubaritem:checked,
    menubaritem:selected,
    popover,
    popover > contents,
    popover.menu,
    popover.menu contents,
    popover modelbutton,
    popover.menu modelbutton,
    popover.menu modelbutton:hover,
    popover.menu modelbutton:selected,
    popover.menu modelbutton:focus,
    modelbutton,
    menuitem {
        border-radius: 0px;
    }

    /* ── Switch: pill toggle matching stra.png design ──────────── */
    switch,
    switch:hover,
    switch:active,
    switch:backdrop {
        border-radius: 10px; /* 20px height / 2 = 10px */
        border: 1px solid alpha(@theme_fg_color, 0.25);
        background-color: alpha(@theme_fg_color, 0.12);
        box-shadow: none;
        outline: none;
    }
    switch:checked,
    switch:checked:hover,
    switch:checked:active,
    switch:checked:backdrop {
        background-color: #2dd4f0;
        border-color: #2dd4f0;
    }
    switch slider,
    switch slider:hover,
    switch slider:active,
    switch slider:backdrop {
        border-radius: 8px; /* 16px diameter / 2 = 8px */
        background-color: #ffffff;
        box-shadow: 0 1px 2px rgba(0, 0, 0, 0.25);
        border: none;
        margin: 2px;
        min-width: 16px;
        min-height: 16px;
    }

    /* ── Context & Speed Color Classes ─────────────────────────── */
    .ctx-green progress {
        background-color: #4ade80;
        border-radius: 4px;
    }
    .ctx-text-green {
        color: #4ade80;
    }

    .ctx-cyan progress {
        background-color: #2dd4f0;
        border-radius: 4px;
    }
    .ctx-text-cyan {
        color: #2dd4f0;
    }

    .ctx-orange progress {
        background-color: #f59e0b;
        border-radius: 4px;
    }
    .ctx-text-orange {
        color: #f59e0b;
    }

    .ctx-red progress {
        background-color: #ef4444;
        border-radius: 4px;
    }
    .ctx-text-red {
        color: #ef4444;
    }

    .accent-label {
        color: #2dd4f0;
        font-weight: bold;
    }

    .dim-label {
        opacity: 0.65;
    }

    /* ── Speed & Stopwatch Labels ────────────────────────── */
    .speed-label {
        color: #2dd4f0;
        font-weight: bold;
    }

    .prompt-speed-label {
        color: #94a3b8;
        font-weight: normal;
    }

    .stopwatch-label {
        color: #64748b;
        font-weight: normal;
    }

    /* ── Bottom Fixed Deck Cards ────────────────────────────── */
    .bottom-deck-card {
        background-color: alpha(@theme_fg_color, 0.05);
        border: 1px solid alpha(@theme_fg_color, 0.10);
        border-radius: 0px;
        padding: 10px 14px;
    }

    .bottom-deck-btn {
        background-color: alpha(@theme_fg_color, 0.06);
        border: 1px solid alpha(#2dd4f0, 0.35);
        border-radius: 0px;
        color: #2dd4f0;
        font-weight: bold;
        font-size: 9pt;
        padding: 4px 10px;
    }

    .bottom-deck-btn:hover {
        background-color: alpha(#2dd4f0, 0.15);
        border-color: #2dd4f0;
    }

    .model-pill {
        background-color: alpha(@theme_fg_color, 0.06);
        border: 1px solid alpha(@theme_fg_color, 0.12);
        border-radius: 0px;
        padding: 2px 8px;
        font-size: 8.5pt;
        color: #94a3b8;
    }

    .model-pill:hover {
        background-color: alpha(@theme_fg_color, 0.10);
        color: @theme_fg_color;
    }

    .model-pill-active {
        background-color: alpha(#2dd4f0, 0.15);
        border: 1px solid #2dd4f0;
        color: #2dd4f0;
        font-weight: bold;
    }

    /* ── Context meter: subtle rounded progress bar ────────────── */
    .context-meter {
        border-radius: 4px;
        background-color: alpha(@theme_fg_color, 0.08);
        min-height: 6px;
    }
    .context-meter progress {
        border-radius: 4px;
    }

    /* ── Log viewer: monospace body ────────────────────────────── */
    .log-viewer-text {
        font-family: monospace;
        font-size: 11pt;
    }

    /* ── Preferences Flat Navigation & Sharp Sizing ───────────── */
    .sidebar-container {
        background-color: alpha(@theme_fg_color, 0.04);
        border: none;
    }
    .sidebar-list {
        background-color: transparent;
        border: none;
    }
    .sidebar-list row {
        border-radius: 0px;
        margin: 0px;
        padding: 0px;
        border: none;
        transition: background-color 100ms ease;
    }
    .sidebar-list row:hover {
        background-color: alpha(@theme_fg_color, 0.08);
    }
    .sidebar-list row:selected {
        background-color: alpha(@theme_fg_color, 0.16);
        color: @theme_fg_color;
    }
    .sidebar-list row:selected:hover {
        background-color: alpha(@theme_fg_color, 0.20);
    }

    /* ── Collapse unused Dialog action area ────────────────────── */
    dialog .dialog-action-area {
        min-height: 0px;
        padding: 0px;
        margin: 0px;
        border: none;
    }

    /* ── Gemini-style Debate Arena Flow ────────────────────────── */
    .arena-stream-container {
        padding: 20px 28px;
    }
    .arena-bubble-user {
        background-color: alpha(@theme_fg_color, 0.08);
        border: 1px solid alpha(#c084fc, 0.35);
        border-radius: 20px;
        padding: 16px 22px;
        margin: 10px 0px 24px 60px;
    }
    .arena-bubble-ai {
        background-color: alpha(@theme_fg_color, 0.04);
        border: 1px solid alpha(@theme_fg_color, 0.08);
        border-radius: 18px;
        padding: 18px 22px;
        margin: 10px 40px 20px 0px;
    }
    .arena-bubble-ai.generator {
        border-left: 4px solid #38bdf8;
    }
    .arena-bubble-ai.auditor {
        border-left: 4px solid #fbbf24;
    }
    .arena-bubble-ai.synthesizer {
        border-left: 4px solid #34d399;
    }
    .arena-bubble-ai.human {
        border-left: 4px solid #c084fc;
    }
    .arena-bubble-ai textview,
    .arena-bubble-ai textview text,
    .arena-bubble-user textview,
    .arena-bubble-user textview text {
        background-color: transparent;
        font-size: 14.5px;
    }
    .arena-code-block {
        background-color: #121215;
        color: #e4e4e7;
        border: 1px solid #27272a;
        border-radius: 14px;
        padding: 16px 18px;
        margin: 12px 0px;
        font-family: monospace;
        font-size: 13.5px;
    }
    .arena-bottom-bar {
        background-color: alpha(@theme_fg_color, 0.05);
        border: 1px solid alpha(@theme_fg_color, 0.12);
        border-radius: 0px;
        padding: 6px 14px;
        margin: 8px 32px 16px 32px;
    }
    .arena-bottom-bar button {
        border-radius: 0px;
    }
    .arena-bottom-entry {
        background-color: transparent;
        border: none;
        border-radius: 0px;
        box-shadow: none;
        font-size: 14px;
        padding: 8px 12px;
    }
"#;

pub fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(CSS);
    if let Some(display) = adw::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
    }
}
