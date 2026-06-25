import { createSignal } from "solid-js";

export interface Profile {
  name: string;
  email: string;
  org: string;
}

const KEY = "cosmog_profile";

function load(): Profile {
  try {
    const raw = localStorage.getItem(KEY);
    if (raw) return JSON.parse(raw) as Profile;
  } catch {}
  return { name: "", email: "", org: "" };
}

export const [profile, setProfile] = createSignal<Profile>(load());

export function saveProfile(p: Profile): void {
  localStorage.setItem(KEY, JSON.stringify(p));
  setProfile(p);
}

export function hasProfile(): boolean {
  return Boolean(load().name);
}
