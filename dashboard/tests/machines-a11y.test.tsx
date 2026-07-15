import { afterEach, describe, expect, it, vi } from "vitest";
import axe from "axe-core";
import { fireEvent, render, screen, cleanup, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import React, { useState } from "react";
import { DestructiveModal } from "../src/machines";

function ModalHarness() {
  const [open, setOpen] = useState(true);
  return (
    <div>
      <button type="button" autoFocus onClick={() => setOpen(true)}>open confirmation</button>
      <p>Background content</p>
      {open ? (
        <DestructiveModal id="confirmation" titleId="confirmation-title" title="Confirm archive" onClose={() => setOpen(false)}>
          <p>This action can be reversed by re-enrollment.</p>
          <button type="button" data-modal-autofocus>confirm</button>
          <button type="button">cancel</button>
        </DestructiveModal>
      ) : null}
    </div>
  );
}

afterEach(() => cleanup());

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

    const result = await axe.run(container, { rules: { "color-contrast": { enabled: false } } });
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
