import SwiftUI

// Pagination pattern inspired by:
// https://medium.engineering/how-to-do-pagination-in-swiftui-04511be7fbd1

// MARK: - Pagination State

enum PaginationState<T> {
    case idle
    case loading
    case empty
    case loaded(items: [T], nextPage: String?)
    case loadingMore(items: [T])
    case error(Error)

    var items: [T] {
        switch self {
        case let .loaded(items, _),
             let .loadingMore(items):
            items
        case .idle,
             .loading,
             .empty,
             .error:
            []
        }
    }

    var hasMore: Bool {
        if case let .loaded(_, nextPage) = self {
            return nextPage != nil
        }
        return false
    }

    var nextPageToken: String? {
        if case let .loaded(_, nextPage) = self {
            return nextPage
        }
        return nil
    }

    var isLoadingMore: Bool {
        if case .loadingMore = self {
            return true
        }
        return false
    }
}

// MARK: - Empty State Configuration

struct EmptyStateConfiguration {
    let title: String
    let systemImage: String
    let description: String
}

// MARK: - Paginated List View

struct PaginatedListView<Item: Identifiable, ItemView: View>: View {
    let empty: EmptyStateConfiguration
    let fetch: (Int, String?) async throws -> (items: [Item], nextPage: String?)
    @ViewBuilder let itemView: (Item) -> ItemView

    @State private var state: PaginationState<Item> = .idle
    @State private var showingError = false
    @State private var loadTask: Task<Void, Never>?

    private let pageSize = 20

    var body: some View {
        content
            .refreshable {
                await refresh()
            }
            .task {
                await loadFirstPage()
            }
            .alert("Error", isPresented: $showingError) {
                Button("OK") {}
            } message: {
                if case let .error(error) = state {
                    Text(error.localizedDescription)
                }
            }
    }

    @ViewBuilder private var content: some View {
        switch state {
        case .idle,
             .loading:
            ProgressView()
                .accessibilityLabel("Loading")

        case .empty:
            ScrollView {
                ContentUnavailableView(
                    empty.title,
                    systemImage: empty.systemImage,
                    description: Text(empty.description)
                )
                .frame(maxWidth: .infinity, minHeight: 300)
            }

        case .loaded,
             .loadingMore:
            List {
                ForEach(state.items) { item in
                    itemView(item)
                }

                if state.hasMore || state.isLoadingMore {
                    HStack {
                        Spacer()
                        ProgressView()
                            .accessibilityLabel("Loading more")
                        Spacer()
                    }
                    .listRowSeparator(.hidden)
                    .onAppear {
                        if state.hasMore {
                            Task { await loadNextPage() }
                        }
                    }
                }
            }

        case .error:
            ScrollView {
                ContentUnavailableView(
                    "Failed to Load",
                    systemImage: "exclamationmark.triangle",
                    description: Text("Pull to refresh and try again")
                )
                .frame(maxWidth: .infinity, minHeight: 300)
            }
        }
    }

    private func loadFirstPage() async {
        // Cancel any existing load to avoid races
        loadTask?.cancel()
        state = .loading
        loadTask = Task {
            await fetchPage(pageToken: nil, existingItems: [])
        }
        await loadTask?.value
    }

    private func refresh() async {
        // Cancel any existing load to avoid races
        loadTask?.cancel()
        // Don't change state - keep current content visible during refresh
        loadTask = Task {
            await fetchPage(pageToken: nil, existingItems: [])
        }
        await loadTask?.value
    }

    private func loadNextPage() async {
        guard case let .loaded(items, nextPage) = state, nextPage != nil else { return }
        state = .loadingMore(items: items)
        await fetchPage(pageToken: nextPage, existingItems: items)
    }

    private func fetchPage(pageToken: String?, existingItems: [Item]) async {
        do {
            let result = try await fetch(pageSize, pageToken)
            let allItems = existingItems + result.items
            if allItems.isEmpty {
                state = .empty
            } else {
                state = .loaded(items: allItems, nextPage: result.nextPage)
            }
        } catch is CancellationError {
            // Task was cancelled, don't update state
        } catch {
            state = .error(error)
            showingError = true
        }
    }
}
