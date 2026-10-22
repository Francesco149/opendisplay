import UIKit

/// The main app's Settings & Help sheet as a grouped table. Rows the legacy
/// build cannot honour are left out: there is no App Store listing to rate or
/// update from, and iOS 12 has no Local Network permission to explain.
final class SettingsViewController: UITableViewController, UITextFieldDelegate {
    private enum Section: Int, CaseIterable {
        case status, name, analytics, diagnostics, howTo, about
    }

    private let model: ReceiverModel
    private let onDone: () -> Void
    private var receiver: StreamReceiver { model.receiver }

    private let howTo = [
        "USB: plug in the cable and open OpenDisplay on your Mac. It connects through the wire on its own (lowest latency).",
        "WiFi: both devices on the same network, then pick this \(deviceKind) in OpenDisplay on your Mac.",
        "Rotate the \(deviceKind) for a vertical second monitor.",
        "Touch: tap to click, drag to drag, two-finger pan to scroll.",
    ]

    init(model: ReceiverModel, onDone: @escaping () -> Void) {
        self.model = model
        self.onDone = onDone
        super.init(style: .grouped)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override func viewDidLoad() {
        super.viewDidLoad()
        title = "OpenDisplay"
        navigationItem.rightBarButtonItem = UIBarButtonItem(barButtonSystemItem: .done,
                                                            target: self, action: #selector(done))
        tableView.rowHeight = UITableView.automaticDimension
        tableView.estimatedRowHeight = 44
        NotificationCenter.default.addObserver(self, selector: #selector(stateChanged),
                                               name: .publishedValueChanged, object: nil)
    }

    @objc private func done() {
        view.endEditing(true)
        dismiss(animated: true, completion: onDone)
    }

    /// Only the status rows track the stream. Reloading just that section
    /// keeps the name field's focus, which a full reload would steal.
    @objc private func stateChanged() {
        DispatchQueue.main.async { [weak self] in
            guard let self, self.isViewLoaded, self.view.window != nil else { return }
            self.tableView.reloadSections(IndexSet(integer: Section.status.rawValue), with: .none)
        }
    }

    private var version: String {
        let info = Bundle.main.infoDictionary
        let short = info?["CFBundleShortVersionString"] as? String ?? "dev"
        let build = info?["CFBundleVersion"] as? String ?? "0"
        return "\(short) (\(build))"
    }

    // MARK: - Table

    override func numberOfSections(in tableView: UITableView) -> Int { Section.allCases.count }

    override func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int {
        switch Section(rawValue: section)! {
        case .status: return receiver.videoSize != .zero ? 3 : 2
        case .name: return 1
        case .analytics: return 2
        case .diagnostics: return 1
        case .howTo: return howTo.count
        case .about: return 3
        }
    }

    override func tableView(_ tableView: UITableView, titleForHeaderInSection section: Int) -> String? {
        switch Section(rawValue: section)! {
        case .status: return "Status"
        case .name: return "Name"
        case .analytics: return "Analytics"
        case .diagnostics: return "Diagnostics"
        case .howTo: return "How to connect"
        case .about: return "About"
        }
    }

    override func tableView(_ tableView: UITableView, titleForFooterInSection section: Int) -> String? {
        switch Section(rawValue: section)! {
        case .name:
            return "Shown in the WiFi list of OpenDisplay on your Mac."
        case .analytics:
            return "The overlay shows FPS, bitrate, frame timing, stalls and latency at the bottom of the screen while streaming. The experimental Metal renderer decodes and presents frames manually; leave it off unless you're debugging."
        case .diagnostics:
            return "What this \(deviceKind) saw while connecting: sessions, restarts, decoder trouble. No screen content, and nothing leaves the \(deviceKind) unless you share it."
        case .about:
            return "The iOS 12 build of OpenDisplay, for devices that can't run the App Store version (iOS 15+)."
        default:
            return nil
        }
    }

    override func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        switch Section(rawValue: indexPath.section)! {
        case .status:
            let rows = [("Listening", "Port 9000"),
                        ("Connection", receiver.connected ? "Connected" : "Waiting for a computer"),
                        ("Stream", "\(Int(receiver.videoSize.width))×\(Int(receiver.videoSize.height)) @ \(receiver.fps) fps")]
            return valueCell(rows[indexPath.row].0, rows[indexPath.row].1)
        case .name:
            return nameCell()
        case .analytics:
            let (title, key) = indexPath.row == 0
                ? ("Performance overlay", "showAnalytics")
                : ("Metal renderer (experimental)", "metalRenderer")
            return switchCell(title, key: key)
        case .diagnostics:
            let cell = UITableViewCell(style: .default, reuseIdentifier: nil)
            cell.textLabel?.text = "Connection log"
            cell.accessoryType = .disclosureIndicator
            return cell
        case .howTo:
            let cell = UITableViewCell(style: .default, reuseIdentifier: nil)
            cell.textLabel?.text = howTo[indexPath.row]
            cell.textLabel?.numberOfLines = 0
            cell.selectionStyle = .none
            return cell
        case .about:
            switch indexPath.row {
            case 0: return valueCell("Version", version)
            case 1: return linkCell("GitHub: peetzweg/opendisplay")
            default: return linkCell("OpenDisplay.app")
            }
        }
    }

    override func tableView(_ tableView: UITableView, didSelectRowAt indexPath: IndexPath) {
        tableView.deselectRow(at: indexPath, animated: true)
        switch (Section(rawValue: indexPath.section)!, indexPath.row) {
        case (.diagnostics, _):
            navigationController?.pushViewController(LogViewController(), animated: true)
        case (.about, 1):
            UIApplication.shared.open(URL(string: "https://github.com/peetzweg/opendisplay")!)
        case (.about, 2):
            UIApplication.shared.open(URL(string: "https://opendisplay.app")!)
        default:
            break
        }
    }

    // MARK: - Cells

    private func valueCell(_ title: String, _ value: String) -> UITableViewCell {
        let cell = UITableViewCell(style: .value1, reuseIdentifier: nil)
        cell.textLabel?.text = title
        cell.detailTextLabel?.text = value
        cell.selectionStyle = .none
        return cell
    }

    private func linkCell(_ title: String) -> UITableViewCell {
        let cell = UITableViewCell(style: .default, reuseIdentifier: nil)
        cell.textLabel?.text = title
        cell.textLabel?.textColor = view.tintColor
        return cell
    }

    private func switchCell(_ title: String, key: String) -> UITableViewCell {
        let cell = UITableViewCell(style: .default, reuseIdentifier: nil)
        cell.textLabel?.text = title
        cell.textLabel?.numberOfLines = 0
        cell.selectionStyle = .none
        let toggle = UISwitch()
        toggle.isOn = UserDefaults.standard.bool(forKey: key)
        toggle.accessibilityIdentifier = key
        toggle.addTarget(self, action: #selector(toggleChanged(_:)), for: .valueChanged)
        cell.accessoryView = toggle
        return cell
    }

    @objc private func toggleChanged(_ toggle: UISwitch) {
        guard let key = toggle.accessibilityIdentifier else { return }
        UserDefaults.standard.set(toggle.isOn, forKey: key)
    }

    private lazy var nameField: UITextField = {
        let field = UITextField()
        field.placeholder = "Device name"
        field.text = UserDefaults.standard.string(forKey: "deviceName") ?? UIDevice.current.name
        field.autocapitalizationType = .words
        field.autocorrectionType = .no
        field.clearButtonMode = .whileEditing
        field.returnKeyType = .done
        field.delegate = self
        field.addTarget(self, action: #selector(nameChanged), for: .editingChanged)
        return field
    }()

    private func nameCell() -> UITableViewCell {
        let cell = UITableViewCell(style: .default, reuseIdentifier: nil)
        cell.selectionStyle = .none
        nameField.removeFromSuperview()
        nameField.translatesAutoresizingMaskIntoConstraints = false
        cell.contentView.addSubview(nameField)
        let margins = cell.contentView.layoutMarginsGuide
        NSLayoutConstraint.activate([
            nameField.leadingAnchor.constraint(equalTo: margins.leadingAnchor),
            nameField.trailingAnchor.constraint(equalTo: margins.trailingAnchor),
            nameField.topAnchor.constraint(equalTo: cell.contentView.topAnchor),
            nameField.bottomAnchor.constraint(equalTo: cell.contentView.bottomAnchor),
            nameField.heightAnchor.constraint(greaterThanOrEqualToConstant: 44),
        ])
        return cell
    }

    @objc private func nameChanged() {
        let name = nameField.text ?? ""
        UserDefaults.standard.set(name, forKey: "deviceName")
        receiver.setServiceName(name)
    }

    func textFieldShouldReturn(_ textField: UITextField) -> Bool {
        textField.resignFirstResponder()
        return true
    }
}

// MARK: - Connection log

/// The main app's DiagnosticsLogView in UIKit: the tail of the log, newest at
/// the bottom, with the whole snapshot one share away.
final class LogViewController: UIViewController {
    /// Same trim as the main app: the shared file carries everything.
    private static let displayedLines = 400

    private let textView = UITextView()
    private var snapshotURL: URL?

    override func viewDidLoad() {
        super.viewDidLoad()
        title = "Connection log"
        view.backgroundColor = .white
        textView.isEditable = false
        textView.font = UIFont(name: "Menlo", size: 11)
        textView.frame = view.bounds
        textView.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        textView.text = "Loading…"
        view.addSubview(textView)
        load()
    }

    private func load() {
        let info = Bundle.main.infoDictionary
        let context = LogSnapshot.Context(
            appVersion: info?["CFBundleShortVersionString"] as? String ?? "dev",
            appBuild: info?["CFBundleVersion"] as? String ?? "0",
            model: DecodeBudget.currentModel,
            systemVersion: UIDevice.current.systemVersion,
            deviceName: UserDefaults.standard.string(forKey: "deviceName") ?? UIDevice.current.name)
        Log.snapshot(context: context) { [weak self] result in
            guard let self else { return }
            guard let (url, text) = result else {
                self.textView.text = "The log could not be read. Restart OpenDisplay and try again."
                return
            }
            self.snapshotURL = url
            self.navigationItem.rightBarButtonItem = UIBarButtonItem(
                barButtonSystemItem: .action, target: self, action: #selector(self.share(_:)))
            let lines = text.split(separator: "\n", omittingEmptySubsequences: false)
            self.textView.text = lines.count > Self.displayedLines
                ? "(showing the last \(Self.displayedLines) lines, the shared file has all of them)\n\n"
                    + lines.suffix(Self.displayedLines).joined(separator: "\n")
                : text
            let end = NSRange(location: max(0, self.textView.text.utf16.count - 1), length: 1)
            self.textView.scrollRangeToVisible(end)
        }
    }

    @objc private func share(_ sender: UIBarButtonItem) {
        guard let url = snapshotURL else { return }
        let sheet = UIActivityViewController(activityItems: [url], applicationActivities: nil)
        sheet.popoverPresentationController?.barButtonItem = sender
        present(sheet, animated: true)
    }
}
