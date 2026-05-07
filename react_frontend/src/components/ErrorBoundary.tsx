import { Component, type ErrorInfo, type ReactNode } from "react";

type ErrorBoundaryProps = {
  children: ReactNode;
};

type ErrorBoundaryState = {
  hasError: boolean;
};

export default class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { hasError: false };

  static getDerivedStateFromError(): ErrorBoundaryState {
    return { hasError: true };
  }

  componentDidCatch(error: Error, errorInfo: ErrorInfo) {
    console.error("Unhandled UI error", error, errorInfo);
  }

  render() {
    if (this.state.hasError) {
      return (
        <main className="workspace-main" role="alert">
          <section className="empty-state">
            <h1>Something went wrong</h1>
            <p>Refresh the page to reload the video library.</p>
          </section>
        </main>
      );
    }

    return this.props.children;
  }
}
