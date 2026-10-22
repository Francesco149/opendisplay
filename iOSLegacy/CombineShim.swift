import Foundation

// The legacy receiver runs on iOS 12, which has no Combine (iOS 13+), yet
// Shared/StreamReceiver.swift and iOS/ReceiverModel.swift declare themselves
// `ObservableObject`s with `@Published` state for the SwiftUI app. These
// module-local stand-ins shadow Combine's names for this target only, so the
// shared files compile unchanged. Instead of publishers, every write posts
// `publishedValueChanged`; the UIKit screens refresh from it.

extension Notification.Name {
    /// Posted on the writing thread whenever any `@Published` value changes.
    /// The shared receiver writes its published state on the main thread.
    static let publishedValueChanged = Notification.Name("publishedValueChanged")
}

protocol ObservableObject: AnyObject {}

@propertyWrapper
struct Published<Value> {
    var wrappedValue: Value {
        didSet { NotificationCenter.default.post(name: .publishedValueChanged, object: nil) }
    }

    init(wrappedValue: Value) {
        self.wrappedValue = wrappedValue
    }
}
