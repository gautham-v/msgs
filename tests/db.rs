//! The read-only database layer, driven against a synthetic fixture.
//!
//! The fixture is built by `tests/fixtures/mod.rs` and carries the real macOS
//! column names. Nothing here opens `~/Library/Messages/chat.db`, and nothing
//! here writes to any database — the last test proves the second half of that.

mod fixtures;

use msgs::app::{App, DbStatus};
use msgs::config::Config;
use msgs::db::{Db, GroupAction, Source, TapbackKind};

fn db() -> Db {
    Db::open(&fixtures::database()).expect("open the fixture read-only")
}

#[test]
fn the_fixture_opens_read_only_and_in_place() {
    let db = db();
    assert_eq!(db.source(), Source::Live);
    assert!(db.scratch_path().is_none());

    let counts = db.counts().expect("counts");
    assert_eq!(counts.chats, 3);
    assert_eq!(counts.handles, 3);
    assert_eq!(counts.attachments, 1);
    // Seven real messages plus four tapback rows.
    assert_eq!(counts.messages, 11);
}

#[test]
fn no_query_path_can_write_to_the_database() {
    let db = db();
    let write = db
        .conn()
        .execute("UPDATE message SET text = 'nope' WHERE ROWID = 1", []);
    assert!(write.is_err(), "a read-only connection must refuse writes");

    let insert = db.conn().execute(
        "INSERT INTO handle (ROWID, id, service) VALUES (99, 'x', 'iMessage')",
        [],
    );
    assert!(
        insert.is_err(),
        "a read-only connection must refuse inserts"
    );
}

#[test]
fn chats_come_back_newest_first_with_participants_and_counts() {
    let chats = db().chats().expect("chats");
    assert_eq!(chats.len(), 3);

    // The group's last message is newer than the direct chat's, and the empty
    // chat sorts last.
    assert_eq!(chats[0].rowid, fixtures::CHAT_GROUP);
    assert_eq!(chats[1].rowid, fixtures::CHAT_DIRECT);
    assert_eq!(chats[2].rowid, fixtures::CHAT_EMPTY);

    let group = &chats[0];
    assert!(group.is_group);
    assert_eq!(group.participants.len(), 3);
    // Participants keep handle rowid order, which the color assignment needs.
    let order: Vec<i64> = group.participants.iter().map(|h| h.rowid).collect();
    assert_eq!(
        order,
        vec![
            fixtures::HANDLE_ALEX,
            fixtures::HANDLE_BAILEY,
            fixtures::HANDLE_CASEY
        ]
    );
    assert_eq!(group.display_name.as_deref(), Some("Fixture Group"));
    assert_eq!(group.message_count, 3);

    let direct = &chats[1];
    assert!(!direct.is_group);
    assert_eq!(direct.participants.len(), 1);
    assert!(direct.display_name.is_none());
    // Tapback rows must not be counted as messages.
    assert_eq!(direct.message_count, 4);

    let empty = &chats[2];
    assert_eq!(empty.message_count, 0);
    assert!(empty.last_message_at().is_none());
}

#[test]
fn unread_counts_only_incoming_unread_messages() {
    let db = db();
    let chats = db.chats().expect("chats");
    let direct = chats
        .iter()
        .find(|chat| chat.rowid == fixtures::CHAT_DIRECT)
        .expect("the direct chat");

    // One incoming message is unread. The outgoing one, the read ones, and the
    // tapback rows do not count.
    assert_eq!(direct.unread_count, 1);
    assert!(direct.is_unread());

    // The group holds one unread reply; its rename event is not a message and
    // must not be counted as unread.
    let group = chats
        .iter()
        .find(|chat| chat.rowid == fixtures::CHAT_GROUP)
        .expect("the group chat");
    assert_eq!(group.unread_count, 1);

    let (total, chats_with_unread) = db.unread_totals().expect("totals");
    assert_eq!(total, 2);
    assert_eq!(chats_with_unread, 2);
}

#[test]
fn is_pinned_is_absent_from_this_schema() {
    let db = db();
    assert!(!db.schema().chat_is_pinned);
    assert!(db.schema().tapback_emoji);
    for chat in db.chats().expect("chats") {
        assert!(chat.is_pinned.is_none());
    }
}

#[test]
fn handles_come_back_in_rowid_order() {
    let db = db();
    let handles = db.handles().expect("handles");
    assert_eq!(handles.len(), 3);
    assert!(handles.windows(2).all(|pair| pair[0].rowid < pair[1].rowid));
    assert!(handles[2].is_email());
    assert!(!handles[0].is_email());

    let map = db.handle_map().expect("handle map");
    assert_eq!(
        map.get(&fixtures::HANDLE_CASEY).map(|h| h.service.as_str()),
        Some("iMessage")
    );
}

#[test]
fn a_conversation_page_is_oldest_first_and_excludes_tapback_rows() {
    let page = db()
        .messages_before(fixtures::CHAT_DIRECT, None, 50)
        .expect("page");

    let rowids: Vec<i64> = page.iter().map(|message| message.rowid).collect();
    assert_eq!(
        rowids,
        vec![
            fixtures::MSG_PLAIN,
            fixtures::MSG_ATTRIBUTED,
            fixtures::MSG_PHOTO,
            fixtures::MSG_UNREAD,
        ]
    );

    let first = &page[0];
    assert!(!first.is_from_me);
    assert_eq!(first.handle_rowid, Some(fixtures::HANDLE_ALEX));
    assert!(first.handle.is_some());
    assert!(first.sent_at().is_some());
    assert!(!first.is_edited);
    assert!(!first.is_announcement());
}

#[test]
fn paging_walks_backwards_through_a_conversation() {
    let db = db();
    let newest = db
        .messages_before(fixtures::CHAT_DIRECT, None, 2)
        .expect("newest page");
    assert_eq!(
        newest.iter().map(|m| m.rowid).collect::<Vec<_>>(),
        vec![fixtures::MSG_PHOTO, fixtures::MSG_UNREAD]
    );

    let older = db
        .messages_before(fixtures::CHAT_DIRECT, Some(newest[0].rowid), 2)
        .expect("older page");
    assert_eq!(
        older.iter().map(|m| m.rowid).collect::<Vec<_>>(),
        vec![fixtures::MSG_PLAIN, fixtures::MSG_ATTRIBUTED]
    );

    let top = db
        .messages_before(fixtures::CHAT_DIRECT, Some(older[0].rowid), 2)
        .expect("above the top");
    assert!(top.is_empty());
}

#[test]
fn new_messages_can_be_fetched_by_watermark() {
    let db = db();
    assert_eq!(db.max_message_rowid().expect("watermark"), 12);

    let since = db
        .messages_after(fixtures::CHAT_DIRECT, fixtures::MSG_ATTRIBUTED, 50)
        .expect("messages after");
    assert_eq!(
        since.iter().map(|m| m.rowid).collect::<Vec<_>>(),
        vec![fixtures::MSG_PHOTO, fixtures::MSG_UNREAD]
    );
}

#[test]
fn a_body_that_only_lives_in_attributed_body_is_recovered() {
    let page = db()
        .messages_before(fixtures::CHAT_DIRECT, None, 50)
        .expect("page");
    let recovered = page
        .iter()
        .find(|message| message.rowid == fixtures::MSG_ATTRIBUTED)
        .expect("the attributedBody message");
    assert_eq!(recovered.text.as_deref(), Some(fixtures::ATTRIBUTED_BODY));
}

#[test]
fn an_attachment_only_message_has_a_file_and_no_body() {
    let page = db()
        .messages_before(fixtures::CHAT_DIRECT, None, 50)
        .expect("page");
    let photo = page
        .iter()
        .find(|message| message.rowid == fixtures::MSG_PHOTO)
        .expect("the photo message");

    assert!(photo.is_from_me);
    // The body was nothing but the attachment placeholder.
    assert!(photo.text.is_none());
    assert!(!photo.is_empty());
    assert_eq!(photo.attachments.len(), 1);

    let file = &photo.attachments[0];
    assert!(file.is_image());
    assert_eq!(file.total_bytes, 2048);
    assert_eq!(file.display_name(), Some("photo.png"));
    assert!(file.path().is_some_and(|path| path.is_absolute()));
    assert!(photo.delivered_at().is_some());
    assert!(photo.read_at().is_some());
}

#[test]
fn tapbacks_land_on_their_target_with_removals_already_applied() {
    let page = db()
        .messages_before(fixtures::CHAT_DIRECT, None, 50)
        .expect("page");
    let target = page
        .iter()
        .find(|message| message.rowid == fixtures::MSG_PLAIN)
        .expect("the reacted-to message");

    // One person liked then unliked then laughed; you loved it. Two stand.
    assert_eq!(target.tapbacks.len(), 2);
    assert_eq!(target.tapbacks[0].kind, TapbackKind::Laughed);
    assert!(!target.tapbacks[0].is_from_me);
    assert_eq!(target.tapbacks[1].kind, TapbackKind::Loved);
    assert!(target.tapbacks[1].is_from_me);

    // Everything else in the page is unreacted.
    assert!(
        page.iter()
            .filter(|message| message.rowid != fixtures::MSG_PLAIN)
            .all(|message| message.tapbacks.is_empty())
    );
}

#[test]
fn a_group_rename_reads_as_an_announcement_not_a_message() {
    let page = db()
        .messages_before(fixtures::CHAT_GROUP, None, 50)
        .expect("page");
    let rename = page
        .iter()
        .find(|message| message.rowid == fixtures::MSG_GROUP_RENAME)
        .expect("the rename event");

    assert!(rename.is_announcement());
    assert_eq!(
        rename.group_action,
        Some(GroupAction::NameChange("Fixture Group".to_string()))
    );

    let reply = page
        .iter()
        .find(|message| message.rowid == fixtures::MSG_GROUP_REPLY)
        .expect("the threaded reply");
    assert_eq!(
        reply.thread_originator_guid.as_deref(),
        Some(fixtures::guid(fixtures::MSG_GROUP_FIRST).as_str())
    );
    assert!(reply.group_action.is_none());
}

#[test]
fn an_empty_chat_pages_to_nothing_rather_than_failing() {
    let page = db()
        .messages_before(fixtures::CHAT_EMPTY, None, 50)
        .expect("page");
    assert!(page.is_empty());
}

#[test]
fn the_app_loads_the_fixture_and_reports_it_on_the_status_line() {
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(fixtures::database());

    assert_eq!(app.status.db, DbStatus::Ready);
    assert!(app.db_error.is_none());
    assert_eq!(app.chat_rows.len(), 3);
    assert_eq!(app.status.unread_total, 2);
    assert_eq!(app.status.unread_chats, 2);

    app.load_conversation(fixtures::CHAT_DIRECT);
    assert_eq!(app.message_rows.len(), 4);
}

#[test]
fn the_app_pages_older_messages_in_above_the_ones_it_has() {
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(fixtures::database());

    let db = app.db.as_ref().expect("an open database");
    app.message_rows = db
        .messages_before(fixtures::CHAT_DIRECT, None, 2)
        .expect("newest page");

    assert_eq!(app.load_older_messages(), 2);
    assert_eq!(app.message_rows.len(), 4);
    assert_eq!(app.message_rows[0].rowid, fixtures::MSG_PLAIN);
    // Nothing left above the top.
    assert_eq!(app.load_older_messages(), 0);
}

#[test]
fn a_missing_database_becomes_a_friendly_error_instead_of_a_crash() {
    let mut app = App::new(Config::default(), Vec::new());
    app.open_db(std::path::PathBuf::from("/nonexistent/msgs/chat.db"));

    assert!(app.db.is_none());
    assert!(app.chat_rows.is_empty());
    let err = app.db_error.as_ref().expect("an error to show");
    assert!(!err.headline().is_empty());
    assert!(err.hint().is_some());
    assert_eq!(app.status.db, DbStatus::Unreadable("not found".to_string()));
}
