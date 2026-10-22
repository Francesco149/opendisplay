import UIKit
import AVFoundation

/// The whole app on one screen: the idle card while no Mac streams, the
/// full-screen `VideoView` once connected and the video format is known.
/// The UIKit counterpart of the main app's `ReceiverScreen`.
final class ReceiverViewController: UIViewController {
    private let model: ReceiverModel
    private var receiver: StreamReceiver { model.receiver }

    private let idleView = IdleView()
    private var videoView: VideoView?
    private let perfLabel = UILabel()
    private var refreshPending = false
    private var shownPeerSignal: PeerUpdateSignal?

    private var isStreaming: Bool { receiver.connected && receiver.videoSize != .zero }
    private var showAnalytics: Bool { UserDefaults.standard.bool(forKey: "showAnalytics") }
    private var metalRenderer: Bool { UserDefaults.standard.bool(forKey: "metalRenderer") }

    init(model: ReceiverModel) {
        self.model = model
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .white

        idleView.frame = view.bounds
        idleView.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        idleView.settingsButton.addTarget(self, action: #selector(openSettings), for: .touchUpInside)
        view.addSubview(idleView)

        perfLabel.numberOfLines = 0
        perfLabel.font = UIFont.monospacedDigitSystemFont(ofSize: 13, weight: .medium)
        perfLabel.textColor = .white
        perfLabel.backgroundColor = UIColor(white: 0, alpha: 0.6)
        perfLabel.layer.cornerRadius = 8
        perfLabel.clipsToBounds = true
        perfLabel.textAlignment = .center
        perfLabel.isUserInteractionEnabled = false   // never block touch input
        perfLabel.isHidden = true
        view.addSubview(perfLabel)

        let center = NotificationCenter.default
        center.addObserver(self, selector: #selector(stateChanged),
                           name: .publishedValueChanged, object: nil)
        center.addObserver(self, selector: #selector(openSettings),
                           name: .deviceDidShake, object: nil)
        refresh()
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        // Rotation re-announces the panel; unchanged sizes are a no-op.
        let size = view.bounds.size
        receiver.setOrientation(portrait: size.height > size.width)
        layoutPerfLabel()
    }

    override var prefersStatusBarHidden: Bool { isStreaming }
    override var prefersHomeIndicatorAutoHidden: Bool { isStreaming }

    // MARK: - State

    /// Published writes arrive in bursts (status, fps and perf together), so
    /// coalesce them into one refresh per main-loop turn.
    @objc private func stateChanged() {
        guard !refreshPending else { return }
        refreshPending = true
        DispatchQueue.main.async { [weak self] in
            self?.refreshPending = false
            self?.refresh()
        }
    }

    private func refresh() {
        let streaming = isStreaming
        if streaming, videoView == nil {
            installVideoView()
        } else if !streaming, let video = videoView {
            video.removeFromSuperview()
            videoView = nil
        }
        // videoSize arrives after the format description: re-fit the layers.
        videoView?.setNeedsLayout()
        idleView.isHidden = streaming
        idleView.update(status: receiver.status, connected: receiver.connected)
        updatePerfLabel(streaming: streaming)
        setNeedsStatusBarAppearanceUpdate()
        if #available(iOS 11, *) { setNeedsUpdateOfHomeIndicatorAutoHidden() }
        presentPeerSignalIfNeeded()
    }

    private func installVideoView() {
        let video = VideoView(frame: view.bounds)
        video.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        video.attach(receiver: receiver, displayLayer: receiver.displayLayer,
                     useMetal: metalRenderer)
        view.insertSubview(video, belowSubview: perfLabel)
        videoView = video
    }

    /// The Metal toggle picks the sink at attach time, as the main app's
    /// `.id(metalRenderer)` does: rebuild the view tree to apply it.
    private func rebuildVideoView() {
        videoView?.removeFromSuperview()
        videoView = nil
        refresh()
    }

    // MARK: - Performance overlay

    private func updatePerfLabel(streaming: Bool) {
        guard streaming, showAnalytics else {
            perfLabel.isHidden = true
            return
        }
        let p = receiver.perf
        let size = receiver.videoSize
        var lines = ["\(Int(size.width))×\(Int(size.height))  \(p.codec)  \(p.transport)",
                     String(format: "%d fps  %.1f Mb/s  frame avg %.0f max %.0f ms",
                            p.fps, p.mbps, p.avgFrameMs, p.maxFrameMs)]
        if p.e2eP50 > 0 {
            lines.append(String(format: "e2e p50 %.0f p95 %.0f ms  rtt %.0f ms",
                                p.e2eP50, p.e2eP95, p.rttMs))
        }
        lines.append("stalls \(p.stalls)  hitches \(p.hitches)  flushes \(p.decodeFlushes)"
                     + "  mac drops \(p.macDrops)")
        perfLabel.text = lines.joined(separator: "\n")
        perfLabel.isHidden = false
        layoutPerfLabel()
    }

    private func layoutPerfLabel() {
        guard !perfLabel.isHidden else { return }
        let width = min(view.bounds.width - 32, 520)
        let fit = perfLabel.sizeThatFits(CGSize(width: width - 16, height: .greatestFiniteMagnitude))
        let size = CGSize(width: min(width, fit.width + 24), height: fit.height + 16)
        perfLabel.frame = CGRect(x: (view.bounds.width - size.width) / 2,
                                 y: view.bounds.height - size.height - 10,
                                 width: size.width, height: size.height)
    }

    // MARK: - Peer compatibility signal (issue #132)

    private func presentPeerSignalIfNeeded() {
        guard let signal = receiver.peerSignal, signal != shownPeerSignal,
              presentedViewController == nil else { return }
        shownPeerSignal = signal
        let message: String
        switch signal {
        case .updateReceiver(let text, _): message = text
        case .updateMac(let text): message = text
        }
        let alert = UIAlertController(title: "Update needed", message: message,
                                      preferredStyle: .alert)
        alert.addAction(UIAlertAction(title: "OK", style: .default))
        present(alert, animated: true)
    }

    // MARK: - Settings

    @objc private func openSettings() {
        guard presentedViewController == nil else { return }
        let metalBefore = metalRenderer
        let settings = SettingsViewController(model: model) { [weak self] in
            guard let self else { return }
            if self.metalRenderer != metalBefore { self.rebuildVideoView() }
            self.refresh()
        }
        let nav = UINavigationController(rootViewController: settings)
        nav.modalPresentationStyle = .formSheet
        present(nav, animated: true)
    }
}

// MARK: - Idle view (no Mac connected)

/// The main app's idle card in UIKit: logo, status, how to connect.
final class IdleView: UIView {
    let settingsButton = UIButton(type: .system)
    private let statusDot = UIView()
    private let statusLabel = UILabel()

    override init(frame: CGRect) {
        super.init(frame: frame)
        backgroundColor = .white

        let logo = UIImageView(image: UIImage(named: "AppLogo"))
        logo.contentMode = .scaleAspectFit
        logo.translatesAutoresizingMaskIntoConstraints = false
        logo.widthAnchor.constraint(equalToConstant: 132).isActive = true
        logo.heightAnchor.constraint(equalToConstant: 132).isActive = true

        let title = UILabel()
        title.text = "OpenDisplay"
        title.font = UIFont.systemFont(ofSize: 34, weight: .bold)

        statusDot.layer.cornerRadius = 4
        statusDot.translatesAutoresizingMaskIntoConstraints = false
        statusDot.widthAnchor.constraint(equalToConstant: 8).isActive = true
        statusDot.heightAnchor.constraint(equalToConstant: 8).isActive = true
        statusLabel.font = UIFont.preferredFont(forTextStyle: .callout)
        statusLabel.textColor = .gray
        let statusRow = UIStackView(arrangedSubviews: [statusDot, statusLabel])
        statusRow.spacing = 8
        statusRow.alignment = .center

        let header = UIStackView(arrangedSubviews: [title, statusRow])
        header.axis = .vertical
        header.alignment = .center
        header.spacing = 6

        let hints = UILabel()
        hints.numberOfLines = 0
        hints.font = UIFont.preferredFont(forTextStyle: .subheadline)
        hints.textColor = .darkGray
        hints.text = [
            "1.  Plug in the USB cable and open OpenDisplay on your Mac",
            "2.  Or pick this \(deviceKind) under WiFi in OpenDisplay on your Mac",
            "3.  Keep this app open. Streaming starts automatically.",
        ].joined(separator: "\n\n")
        let card = UIView()
        card.backgroundColor = UIColor(white: 0.95, alpha: 1)
        card.layer.cornerRadius = 16
        hints.translatesAutoresizingMaskIntoConstraints = false
        card.addSubview(hints)
        NSLayoutConstraint.activate([
            hints.topAnchor.constraint(equalTo: card.topAnchor, constant: 20),
            hints.bottomAnchor.constraint(equalTo: card.bottomAnchor, constant: -20),
            hints.leadingAnchor.constraint(equalTo: card.leadingAnchor, constant: 20),
            hints.trailingAnchor.constraint(equalTo: card.trailingAnchor, constant: -20),
            card.widthAnchor.constraint(lessThanOrEqualToConstant: 420),
        ])

        settingsButton.setTitle("Settings & Help", for: .normal)
        settingsButton.titleLabel?.font = UIFont.preferredFont(forTextStyle: .body)

        let tip = UILabel()
        tip.text = "Tip: shake the \(deviceKind) to open settings anytime"
        tip.font = UIFont.preferredFont(forTextStyle: .footnote)
        tip.textColor = .lightGray

        let stack = UIStackView(arrangedSubviews: [logo, header, card, settingsButton, tip])
        stack.axis = .vertical
        stack.alignment = .center
        stack.spacing = 28
        stack.setCustomSpacing(16, after: settingsButton)
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.centerXAnchor.constraint(equalTo: centerXAnchor),
            stack.centerYAnchor.constraint(equalTo: centerYAnchor),
            stack.leadingAnchor.constraint(greaterThanOrEqualTo: leadingAnchor, constant: 20),
            stack.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -20),
        ])
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    func update(status: String, connected: Bool) {
        statusLabel.text = status
        statusDot.backgroundColor = connected ? .green : .orange
    }
}
