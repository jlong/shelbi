"use client"

import * as UIMenu from "@radix-ui/react-dropdown-menu"
import { MouseEventHandler, ReactNode } from "react"

export const Menu = ({
  onOpenChange,
  children,
}: {
  onOpenChange?: (open: boolean) => void
  children: ReactNode | ReactNode[]
}) => <UIMenu.Root onOpenChange={onOpenChange}>{children}</UIMenu.Root>

export const MenuTrigger = ({
  className,
  children,
}: {
  className?: string
  children: ReactNode | ReactNode[]
}) => (
  <UIMenu.Trigger
    className={`flex items-center justify-center outline-none cursor-pointer ${className ?? ""}`}
  >
    {children}
  </UIMenu.Trigger>
)

export const MenuContent = ({
  align = "start",
  className,
  children,
}: {
  align?: "center" | "start" | "end"
  className?: string
  children: ReactNode | ReactNode[]
}) => (
  <UIMenu.Portal>
    <UIMenu.Content
      className={`min-w-[140px] rounded-md border border-gray-4 bg-gray-1 shadow-lg py-0.5 z-50 ${className ?? ""}`}
      align={align}
      sideOffset={4}
      collisionPadding={8}
    >
      {children}
    </UIMenu.Content>
  </UIMenu.Portal>
)

export const MenuButton = ({
  className,
  onClick,
  children,
}: {
  className?: string
  onClick?: MouseEventHandler<HTMLButtonElement>
  children: ReactNode | ReactNode[]
}) => (
  <UIMenu.Item asChild>
    <button
      className={`flex w-full items-center gap-1 px-1.5 py-0.5 font-mono text-sm text-gray-7 hover:text-fg hover:bg-gray-2 cursor-pointer outline-none ${className ?? ""}`}
      onClick={onClick}
    >
      {children}
    </button>
  </UIMenu.Item>
)
