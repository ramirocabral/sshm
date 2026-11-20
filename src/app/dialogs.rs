use tokio::sync::oneshot;

use super::form::{Choice, Field, FieldKind, Form, Mode};
use super::widgets::TextInput;
use crate::secrets::SecretKind;
use crate::ssh::command::PromptKind;
use crate::store::hosts::{Host, SecretSource, normalize_folder, subpath};
use crate::term::pane::PaneId;

pub enum Dialog {
    Unlock(UnlockDialog),
    AskPass(AskPassDialog),
    Confirm(ConfirmDialog),
    HostForm(Box<HostForm>),
    Input(InputDialog),
    Message { title: String, lines: Vec<String> },
    Help(HelpDialog),
    Palette(PaletteDialog),
    Import(ImportForm),
    Export(ExportForm),
    Move(MoveForm),
    Theme(ThemeForm),
}

/// What to do once a secret is available.
pub enum SecretPurpose {
    Type { pane: PaneId },
    AskPass { reply: oneshot::Sender<Option<String>>, prompt: String, host_id: String, label: String, kind: SecretKind },
}

pub enum AfterUnlock {
    Nothing,
    Secret { host_id: String, kind: SecretKind, purpose: SecretPurpose },
    SaveSecrets { host_id: String, secrets: Vec<(SecretKind, Option<String>)> },
    Import(ImportForm),
    Export(ExportForm),
    ChangeMaster,
}

pub struct UnlockDialog {
    pub input: TextInput,
    pub confirm: TextInput,
    pub creating: bool,
    pub changing: bool,
    pub field: usize,
    pub error: Option<String>,
    pub then: AfterUnlock,
}

impl UnlockDialog {
    pub fn new(creating: bool, then: AfterUnlock) -> Self {
        UnlockDialog { input: TextInput::masked(), confirm: TextInput::masked(), creating, changing: false, field: 0, error: None, then }
    }
}

pub struct AskPassDialog {
    pub label: String,
    pub prompt: String,
    pub input: TextInput,
    pub kind: PromptKind,
    pub host_id: Option<String>,
    pub secret_kind: Option<SecretKind>,
    pub save: bool,
    pub note: Option<String>,
    pub reply: Option<oneshot::Sender<Option<String>>>,
}

pub enum ConfirmAction {
    HostKey(oneshot::Sender<Option<String>>),
    DeleteHosts(Vec<String>),
    ClosePane(PaneId),
    Quit,
    SftpDelete { tab: u64, remote: bool, paths: Vec<String> },
    SftpOverwrite { tab: u64, upload: bool, sources: Vec<String> },
}

pub struct ConfirmDialog {
    pub title: String,
    pub body: String,
    pub action: Option<ConfirmAction>,
}

pub enum InputAction {
    SftpMkdir { tab: u64, remote: bool },
    SftpRename { tab: u64, remote: bool, from: String },
    SftpGoto { tab: u64, remote: bool },
    RenameTab { tab: u64 },
    RenameFolder { from: String },
}

pub struct InputDialog {
    pub title: String,
    pub input: TextInput,
    pub action: Option<InputAction>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    NewHost,
    LocalShell,
    ImportSshConfig,
    Import,
    Export,
    LockVault,
    ChangeMaster,
    ToggleBroadcast,
    ToggleSidebar,
    Theme,
    Help,
    Paths,
    Quit,
}

impl Command {
    pub const ALL: [Command; 13] = [
        Command::NewHost,
        Command::LocalShell,
        Command::ImportSshConfig,
        Command::Import,
        Command::Export,
        Command::LockVault,
        Command::ChangeMaster,
        Command::ToggleBroadcast,
        Command::ToggleSidebar,
        Command::Theme,
        Command::Help,
        Command::Paths,
        Command::Quit,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Command::NewHost => "New host",
            Command::LocalShell => "Open a local shell",
            Command::ImportSshConfig => "Import ~/.ssh/config",
            Command::Import => "Import from file… (JSON, CSV, bundle, MobaXterm, Ásbrú, Remmina)",
            Command::Export => "Export hosts…",
            Command::LockVault => "Lock the vault",
            Command::ChangeMaster => "Change the master password",
            Command::ToggleBroadcast => "Toggle broadcast in this tab",
            Command::ToggleSidebar => "Show/hide the host list",
            Command::Theme => "Change the theme",
            Command::Help => "Help and key bindings",
            Command::Paths => "Show config and data locations",
            Command::Quit => "Quit",
        }
    }
}

pub struct PaletteDialog {
    pub input: TextInput,
    pub selected: usize,
}

impl PaletteDialog {
    pub fn matches(&self) -> Vec<Command> {
        let q = self.input.value.to_lowercase();
        Command::ALL.iter().copied().filter(|c| q.split_whitespace().all(|w| c.label().to_lowercase().contains(w))).collect()
    }
}

#[derive(Default)]
pub struct HelpDialog {
    pub scroll: u16,
    pub search: Option<TextInput>,
    pub query: String,
}

pub fn default_import_path(source: &str) -> &'static str {
    match source {
        "ssh-config" => "~/.ssh/config",
        "json" => "~/sshm-export.json",
        "csv" => "~/sshm-export.csv",
        "bundle" => "~/sshm-export.age",
        "mobaxterm" => "~/MobaXterm.ini",
        "asbru" => "~/.config/asbru/asbru.yml",
        _ => "~/.local/share/remmina",
    }
}

pub struct ImportForm {
    pub form: Form,
}

impl ImportForm {
    pub fn new() -> Self {
        let sources = vec![
            Choice::new("ssh-config", "OpenSSH config", "Host blocks of ~/.ssh/config"),
            Choice::new("json", "sshm JSON", "an export made with sshm"),
            Choice::new("csv", "sshm CSV", "name,folder,host,port,user,… (spreadsheet friendly)"),
            Choice::new("bundle", "Encrypted bundle", "an sshm export protected with a password"),
            Choice::new("mobaxterm", "MobaXterm", "MobaXterm.ini or a .mxtsessions file"),
            Choice::new("asbru", "Ásbrú", "asbru.yml"),
            Choice::new("remmina", "Remmina", "a .remmina file or the whole directory"),
        ];
        let conflicts = vec![
            Choice::new("skip", "Skip it", "keep the existing host untouched"),
            Choice::new("overwrite", "Overwrite it", "replace the existing host with the imported one"),
            Choice::new("rename", "Import as a copy", "add it with a \"(2)\" suffix"),
        ];
        let fields = vec![
            Field::select("source", "Import from", sources, "ssh-config").hint("Where the hosts come from"),
            Field::text("path", "File", default_import_path("ssh-config")).hint("Path to the file; ~ is expanded"),
            Field::secret("passphrase", "Password", false).placeholder("type it here"),
            Field::select("conflict", "If it exists", conflicts, "skip").hint("Hosts with the same folder and name"),
        ];
        let mut f = ImportForm { form: Form::new("Import hosts", "Import", fields) };
        f.sync();
        f
    }

    pub fn on_change(&mut self, id: &str) {
        if id == "source" {
            let src = self.form.value("source");
            self.form.set_value("path", default_import_path(&src));
        }
        self.sync();
    }

    fn sync(&mut self) {
        let src = self.form.value("source");
        let hint = match src.as_str() {
            "bundle" => "The password used when the bundle was exported",
            "mobaxterm" => "MobaXterm master password, to bring the saved passwords along (empty = hosts only)",
            _ => "",
        };
        self.form.field_mut("passphrase").hint = hint.into();
        self.form.set_visible("passphrase", !hint.is_empty());
    }
}

pub enum MoveWhat {
    Folder(String),
    Hosts(Vec<String>),
}

pub struct MoveForm {
    pub form: Form,
    pub what: MoveWhat,
}

impl MoveForm {
    /// `current` is the destination it starts on; `folders` are offered as destinations, except
    /// the folder being moved and the ones below it.
    pub fn new(what: MoveWhat, title: String, current: &str, folders: Vec<String>) -> Self {
        let inside = |f: &str| matches!(&what, MoveWhat::Folder(moving) if subpath(f, moving).is_some());
        let mut dests: Vec<Choice> =
            folders.into_iter().filter(|f| !inside(f)).map(|f| Choice::new(f.clone(), f, "")).collect();
        dests.insert(0, Choice::new("", "(top level)", "no folder"));
        let fields = vec![
            Field::combo("dest", "Move to", current, dests)
                .hint("Press Enter to pick a folder, or type a new one (a/b for subfolders)"),
        ];
        MoveForm { form: Form::new(title, "Move", fields), what }
    }
}

/// Picks a theme; every change is shown at once, Cancel goes back to `previous`.
pub struct ThemeForm {
    pub form: Form,
    pub previous: String,
}

impl ThemeForm {
    pub fn new(current: &str) -> Self {
        let themes = crate::ui::theme::THEMES.iter().map(|t| Choice::new(t.name, t.name, t.description)).collect();
        let fields = vec![
            Field::select("theme", "Theme", themes, current)
                .hint("Enter opens the list; the colors change as you move. Saved to config.toml"),
        ];
        ThemeForm { form: Form::new("Theme", "Save", fields), previous: current.to_string() }
    }
}

pub struct ExportForm {
    pub form: Form,
}

impl ExportForm {
    pub fn new() -> Self {
        let formats = vec![
            Choice::new("bundle", "Encrypted bundle", "hosts + passwords, protected with a password (recommended)"),
            Choice::new("json", "JSON", "plain text"),
            Choice::new("csv", "CSV", "plain text, opens in a spreadsheet"),
        ];
        let fields = vec![
            Field::select("format", "Format", formats, "bundle").hint("The bundle is the way to move everything to another machine"),
            Field::text("path", "File", "~/sshm-export.age").hint("Where to write the export; ~ is expanded"),
            Field::toggle("with_secrets", "Include passwords", true),
            Field::secret("passphrase", "Bundle password", false)
                .placeholder("choose a password")
                .hint("Needed to import the bundle later"),
            Field::secret("confirm", "Repeat password", false).placeholder("type it again"),
        ];
        let mut f = ExportForm { form: Form::new("Export hosts", "Export", fields) };
        f.sync();
        f
    }

    pub fn on_change(&mut self, id: &str) {
        if id == "format" {
            let ext = match self.form.value("format").as_str() {
                "json" => "json",
                "csv" => "csv",
                _ => "age",
            };
            let path = self.form.value("path");
            let base = path.rsplit_once('.').map(|(b, _)| b.to_string()).unwrap_or(path);
            self.form.set_value("path", &format!("{base}.{ext}"));
        }
        self.sync();
    }

    fn sync(&mut self) {
        let bundle = self.form.value("format") == "bundle";
        self.form.set_visible("passphrase", bundle);
        self.form.set_visible("confirm", bundle);
        self.form.field_mut("with_secrets").hint = if bundle {
            "Passwords go inside the encrypted bundle".into()
        } else {
            "Careful: passwords end up in plain text in the file".into()
        };
    }
}

pub struct HostFormContext {
    pub folders: Vec<String>,
    pub jump_hosts: Vec<Choice>,
    pub keys: Vec<Choice>,
    pub saved_password: bool,
    pub saved_root: bool,
    /// The vault exists but is locked, so whether a password is stored is unknown.
    pub vault_locked: bool,
}

pub struct HostForm {
    pub form: Form,
    pub id: String,
    pub editing: bool,
}

fn source_choices() -> Vec<Choice> {
    vec![
        Choice::new("keyring", "System keyring", "GNOME Keyring, KWallet, KeePassXC… (secret-tool)"),
        Choice::new("vault", "Saved in sshm", "stored encrypted in sshm's vault"),
        Choice::new("none", "Not saved", "keys only, or type it when asked"),
        Choice::new("bw", "Bitwarden (bw)", "official Bitwarden CLI, needs BW_SESSION"),
        Choice::new("rbw", "Bitwarden (rbw)", "rbw CLI with its own agent"),
        Choice::new("op", "1Password", "1Password CLI secret reference"),
        Choice::new("cmd", "Command", "any command that prints the password"),
    ]
}

fn split_source(s: &SecretSource) -> (&'static str, String) {
    match s {
        SecretSource::Vault => ("vault", String::new()),
        SecretSource::Keyring => ("keyring", String::new()),
        SecretSource::None => ("none", String::new()),
        SecretSource::Bitwarden { item } => ("bw", item.clone()),
        SecretSource::Rbw { item } => ("rbw", item.clone()),
        SecretSource::OnePassword { reference } => ("op", reference.clone()),
        SecretSource::Command { command } => ("cmd", command.clone()),
    }
}

/// Field ids for one password block: (source, vault value, keyring value, bitwarden item, 1password ref, command).
type SourceIds = (&'static str, &'static str, &'static str, &'static str, &'static str, &'static str);
const PW: SourceIds = ("pw_source", "pw_vault", "pw_keyring", "pw_item", "pw_ref", "pw_cmd");
const ROOT: SourceIds = ("root_source", "root_vault", "root_keyring", "root_item", "root_ref", "root_cmd");

impl HostForm {
    pub fn new(host: &Host, editing: bool, ctx: HostFormContext) -> Self {
        let opt = |o: &Option<String>| o.clone().unwrap_or_default();
        let mut folders: Vec<Choice> = ctx.folders.iter().map(|f| Choice::new(f.clone(), f.clone(), "")).collect();
        folders.insert(0, Choice::new("", "(top level)", "no folder"));
        let (pw_kind, pw_value) = split_source(&host.password);
        let (root_kind, root_value) = split_source(&host.root_password);
        let value_for = |kind: &str, wanted: &[&str], v: &str| if wanted.contains(&kind) { v.to_string() } else { String::new() };

        let mut fields = vec![
            Field::text("name", "Name", host.name.clone()).section("Connection").hint("How the host shows up in the list"),
            Field::combo("folder", "Folder", host.folder.clone(), folders)
                .placeholder("(top level)")
                .hint("Type a folder (a/b for subfolders) or press Enter to pick an existing one"),
            Field::text("host", "Host", host.host.clone()).placeholder("required").hint("IP, hostname, or an alias from ~/.ssh/config"),
            Field::text("port", "Port", host.port.map(|p| p.to_string()).unwrap_or_default())
                .placeholder("22")
                .hint("Empty = the port from ~/.ssh/config, or 22"),
            Field::text("user", "User", opt(&host.user)).placeholder("from ~/.ssh/config").hint("Empty = the user from ~/.ssh/config, or yours"),
            Field::combo("jump", "Jump host", opt(&host.jump), ctx.jump_hosts)
                .placeholder("none")
                .hint("Connect through another host (ProxyJump). Enter lists your hosts, or type user@host:port"),
            Field::combo("identity", "Private key", opt(&host.identity_file), ctx.keys)
                .section("Authentication")
                .placeholder("ssh-agent / ~/.ssh/config")
                .hint("Enter lists the keys in ~/.ssh, or type a path. Empty = whatever ssh would use"),
        ];
        for (ids, label, kind, value, stored, what) in [
            (PW, "Password", pw_kind, &pw_value, ctx.saved_password, "the login password; also used for sudo"),
            (ROOT, "Root password", root_kind, &root_value, ctx.saved_root, "for su - (leader+P types it)"),
        ] {
            let (src, vault, keyring, item, reference, cmd) = ids;
            fields.push(Field::select(src, label, source_choices(), kind).hint(format!("Where {what} comes from")));
            fields.push(
                Field::secret(vault, "  Saved password", stored)
                    .placeholder(if ctx.vault_locked { "vault locked · leave empty to keep the current one" } else { "" })
                    .hint("Stored encrypted in the vault"),
            );
            fields.push(
                Field::secret(keyring, "  Saved password", false)
                    .placeholder(if editing { "leave empty to keep the current one" } else { "" })
                    .hint("Stored in the system keyring; type it to replace the saved one"),
            );
            fields.push(
                Field::text(item, "  Bitwarden item", value_for(kind, &["bw", "rbw"], value))
                    .placeholder("item name or id")
                    .hint("Name or id of the Bitwarden item that holds the password"),
            );
            fields.push(
                Field::text(reference, "  1Password ref", value_for(kind, &["op"], value))
                    .placeholder("op://vault/item/password")
                    .hint("Secret reference, as shown by 'Copy Secret Reference' in 1Password"),
            );
            fields.push(
                Field::text(cmd, "  Command", value_for(kind, &["cmd"], value))
                    .placeholder("pass show ssh/web1")
                    .hint("Run with sh -c; its output is the password"),
            );
        }
        fields.extend([
            Field::toggle("auto_sudo", "Auto sudo", host.auto_sudo)
                .section("Behavior")
                .hint("Type the password into sudo prompts automatically (once per prompt)"),
            Field::text("on_connect", "On connect", opt(&host.on_connect))
                .placeholder("e.g. sudo -i")
                .hint("Typed into the session once the shell prompt shows up"),
            Field::text("ssh_args", "Extra ssh args", host.ssh_args.join(" "))
                .placeholder("e.g. -o ServerAliveInterval=30")
                .hint("Passed to ssh as is"),
            Field::text("tags", "Tags", host.tags.join(", ")).section("Notes").placeholder("e.g. prod, web").hint("Comma separated; / in the host list searches them"),
            Field::text("notes", "Notes", host.notes.clone()),
        ]);
        let title = if editing { format!("Edit host · {}", host.name) } else { "New host".to_string() };
        let mut f = HostForm { form: Form::new(title, "Save", fields), id: host.id.clone(), editing };
        if !editing {
            f.form.mode = Mode::Insert;
        }
        f.sync();
        f
    }

    pub fn on_change(&mut self, _id: &str) {
        self.sync();
    }

    fn sync(&mut self) {
        for (src, vault, keyring, item, reference, cmd) in [PW, ROOT] {
            let kind = self.form.value(src);
            self.form.set_visible(vault, kind == "vault");
            self.form.set_visible(keyring, kind == "keyring");
            self.form.set_visible(item, kind == "bw" || kind == "rbw");
            self.form.set_visible(reference, kind == "op");
            self.form.set_visible(cmd, kind == "cmd");
        }
    }

    fn source(&self, ids: SourceIds, what: &str) -> Result<SecretSource, String> {
        let (src, _, _, item, reference, cmd) = ids;
        Ok(match self.form.value(src).as_str() {
            "none" => SecretSource::None,
            "keyring" => SecretSource::Keyring,
            "bw" | "rbw" => {
                let item = self.form.value(item);
                if item.is_empty() {
                    return Err(format!("{what}: the Bitwarden item is required"));
                }
                if self.form.value(src) == "bw" { SecretSource::Bitwarden { item } } else { SecretSource::Rbw { item } }
            }
            "op" => {
                let reference = self.form.value(reference);
                if !reference.starts_with("op://") {
                    return Err(format!("{what}: the 1Password reference must look like op://vault/item/field"));
                }
                SecretSource::OnePassword { reference }
            }
            "cmd" => {
                let command = self.form.value(cmd);
                if command.is_empty() {
                    return Err(format!("{what}: the command is required"));
                }
                SecretSource::Command { command }
            }
            _ => SecretSource::Vault,
        })
    }

    pub fn to_host(&self) -> Result<Host, String> {
        let f = &self.form;
        let host = f.value("host");
        if host.is_empty() {
            return Err("the Host field is required".into());
        }
        let port = match f.value("port") {
            p if p.is_empty() => None,
            p => Some(p.parse::<u16>().map_err(|_| format!("invalid port: {p}"))?),
        };
        let opt = |id: &str| Some(f.value(id)).filter(|v| !v.is_empty());
        let name = f.value("name");
        let folder = normalize_folder(&f.value("folder"));
        Ok(Host {
            id: self.id.clone(),
            name: if name.is_empty() { host.clone() } else { name },
            folder,
            host,
            port,
            user: opt("user"),
            identity_file: opt("identity"),
            jump: opt("jump"),
            tags: f.value("tags").split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect(),
            notes: f.value("notes"),
            password: self.source(PW, "Password")?,
            root_password: self.source(ROOT, "Root password")?,
            auto_sudo: f.toggled("auto_sudo"),
            on_connect: opt("on_connect"),
            ssh_args: f.value("ssh_args").split_whitespace().map(str::to_string).collect(),
        })
    }

    /// Passwords typed for the chosen source (vault or keyring): Some(value) stores, None
    /// deletes; untouched fields are left out.
    pub fn secret_changes(&self) -> Vec<(SecretKind, Option<String>)> {
        let mut out = Vec::new();
        for (ids, kind) in [(PW, SecretKind::Password), (ROOT, SecretKind::RootPassword)] {
            let id = match self.form.value(ids.0).as_str() {
                "vault" => ids.1,
                "keyring" => ids.2,
                _ => continue,
            };
            if let FieldKind::Secret { input, cleared, .. } = &self.form.field(id).kind {
                if !input.value.is_empty() {
                    out.push((kind, Some(input.value.clone())));
                } else if *cleared {
                    out.push((kind, None));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> HostFormContext {
        HostFormContext {
            folders: vec!["a".into(), "a/b".into()],
            jump_hosts: vec![],
            keys: vec![],
            saved_password: false,
            saved_root: false,
            vault_locked: false,
        }
    }

    #[test]
    fn host_form_roundtrip() {
        let host = Host {
            name: "web".into(),
            folder: "a/b".into(),
            host: "10.1.1.1".into(),
            port: Some(22),
            user: Some("root".into()),
            tags: vec!["prod".into(), "web".into()],
            auto_sudo: true,
            ssh_args: vec!["-o".into(), "A=B".into()],
            password: SecretSource::Rbw { item: "web1".into() },
            root_password: SecretSource::OnePassword { reference: "op://i/w/password".into() },
            ..Host::default()
        };
        let mut form = HostForm::new(&host, true, ctx());
        assert_eq!(form.to_host().unwrap(), host);
        assert!(form.form.field("pw_item").visible);
        assert!(!form.form.field("pw_vault").visible);

        form.form.set_value("pw_source", "vault");
        form.on_change("pw_source");
        assert!(form.form.field("pw_vault").visible);
        assert!(!form.form.field("pw_item").visible);
        form.form.set_value("pw_vault", "s3cret");
        assert_eq!(form.secret_changes(), vec![(SecretKind::Password, Some("s3cret".into()))]);
        assert_eq!(form.to_host().unwrap().password, SecretSource::Vault);

        form.form.set_value("port", "abc");
        assert!(form.to_host().is_err());
        form.form.set_value("port", "");
        form.form.set_value("root_source", "cmd");
        form.on_change("root_source");
        assert!(form.to_host().unwrap_err().contains("command is required"));
    }

    #[test]
    fn import_and_export_forms() {
        let mut i = ImportForm::new();
        assert!(!i.form.field("passphrase").visible);
        i.form.set_value("source", "bundle");
        i.on_change("source");
        assert!(i.form.field("passphrase").visible);
        assert_eq!(i.form.value("path"), "~/sshm-export.age");

        let mut e = ExportForm::new();
        e.form.set_value("format", "csv");
        e.on_change("format");
        assert_eq!(e.form.value("path"), "~/sshm-export.csv");
        assert!(!e.form.field("passphrase").visible);
    }
}
