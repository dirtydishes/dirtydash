import { afterEach, describe, expect, it, vi } from "vitest";
import axe from "axe-core";
import { fireEvent, render, screen, cleanup, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import React, { useState } from "react";
import { DestructiveModal, MachinesWorkspace } from "../src/machines";

function ModalHarness() {
  const [open, setOpen] = useState(true);
  return (
    <main>
      <button type="button" autoFocus onClick={() => setOpen(true)}>open confirmation</button>
      <p>Background content</p>
      {open ? (
        <DestructiveModal id="confirmation" titleId="confirmation-title" title="Confirm archive" onClose={() => setOpen(false)}>
          <p>This action can be reversed by re-enrollment.</p>
          <button type="button" data-modal-autofocus>confirm</button>
          <button type="button">cancel</button>
        </DestructiveModal>
      ) : null}
    </main>
  );
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function jsonResponse(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" }
  });
}

function renderedReactOnClickSource(element: HTMLElement) {
  const reactPropsKey = Object.keys(element).find((key) => key.startsWith("__reactProps$"));
  expect(reactPropsKey).toBeDefined();
  const props = (element as unknown as Record<string, { onClick?: unknown }>)[reactPropsKey!];
  expect(typeof props.onClick).toBe("function");
  return String(props.onClick);
}

const hostTrustDraft = {
  id: "enroll-probe-secret",
  machine_id: "machine-probe-secret",
  display_name: "Probe Secret Machine",
  state: "host-trust-auth",
  blocker: "none",
  execution_substate: "not-started",
  cleanup_complete: true,
  updated_at: "2026-07-15T00:00:00Z"
};

describe("enrollment secret retry handling", () => {
  it("clears probe credentials and renders a retry closure that cannot capture the request body", async () => {
    const user = userEvent.setup();
    const probeBodies: string[] = [];
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = input.toString();
      if (url.endsWith("/api/v1/admin/session/csrf")) return jsonResponse({ csrf_token: "csrf-token" });
      if (url.endsWith("/api/v1/admin/machines")) return jsonResponse([]);
      if (url.endsWith("/api/v1/admin/updates")) return jsonResponse([]);
      if (url.endsWith("/api/v1/admin/enrollment")) return jsonResponse([hostTrustDraft]);
      if (url.endsWith("/api/v1/admin/enrollment/enroll-probe-secret/probe")) {
        probeBodies.push(String(init?.body ?? ""));
        return jsonResponse({ message: "probe failed" }, 500);
      }
      return jsonResponse({ message: `unexpected ${url}` }, 404);
    }));

    render(<MachinesWorkspace activeTab="enroll" />);
    const probe = await screen.findByRole("button", { name: /probe \+ plan/i });
    const sshPassword = screen.getByLabelText(/SSH password/i) as HTMLInputElement;
    const sudoPassword = screen.getByLabelText(/sudo password/i) as HTMLInputElement;

    await user.type(sshPassword, "ssh-secret-sentinel");
    await user.type(sudoPassword, "sudo-secret-sentinel");
    await user.click(probe);

    await waitFor(() => expect(probeBodies).toHaveLength(1));
    expect(sshPassword.value).toBe("");
    expect(sudoPassword.value).toBe("");
    expect(probeBodies[0]).toContain("ssh-secret-sentinel");
    expect(probeBodies[0]).toContain("sudo-secret-sentinel");

    const retryButton = await screen.findByRole("button", { name: "retry" });
    const retryHandlerSource = renderedReactOnClickSource(retryButton);
    expect(retryHandlerSource).toMatch(/step\(path, \{\}\)/);
    expect(retryHandlerSource).not.toMatch(/\bbody\b/);
    expect(retryHandlerSource).not.toMatch(/carriesSecret/);

    await user.click(retryButton);
    await waitFor(() => expect(probeBodies).toHaveLength(2));
    expect(probeBodies[1]).not.toContain("ssh-secret-sentinel");
    expect(probeBodies[1]).not.toContain("sudo-secret-sentinel");
    expect(JSON.parse(probeBodies[1])).toEqual({});
  });
});

describe("destructive confirmation dialog", () => {
  it("is modal, traps focus, makes the background inert, and restores the trigger", async () => {
    const { container } = render(<ModalHarness />);
    const dialog = await screen.findByRole("dialog", { name: "Confirm archive" });
    const confirm = screen.getByRole("button", { name: "confirm" });
    const cancel = screen.getByRole("button", { name: "cancel" });
    const trigger = screen.getByRole("button", { name: "open confirmation" });

    expect(dialog.getAttribute("aria-modal")).toBe("true");
    expect(document.activeElement).toBe(confirm);
    expect((trigger as HTMLElement & { inert?: boolean }).inert).toBe(true);

    fireEvent.keyDown(cancel, { key: "Tab" });
    expect(document.activeElement).toBe(confirm);
    fireEvent.keyDown(confirm, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(cancel);

    fireEvent.keyDown(dialog, { key: "Escape" });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(document.activeElement).toBe(trigger);
    expect((trigger as HTMLElement & { inert?: boolean }).inert).toBe(false);

    const result = await axe.run(document.body, { rules: { "color-contrast": { enabled: false } } });
    expect(result.violations).toEqual([]);
  });

  it("closes from the labelled backdrop without treating it as a content control", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(
      <DestructiveModal id="confirmation" titleId="confirmation-title" title="Confirm delete" onClose={onClose}>
        <button type="button">cancel</button>
      </DestructiveModal>
    );

    await user.click(screen.getByRole("button", { name: "Close confirmation" }));
    expect(onClose).toHaveBeenCalledOnce();
  });
});
