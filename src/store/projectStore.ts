import { create } from "zustand";

export interface Project {
  name: string;
  description?: string;
  filePath?: string;
  connectionIds: string[];
  dashboardOpen: boolean;
  autoReconnect: boolean;
  createdAt?: string;
  lastModified?: string;
}

interface ProjectStore {
  currentProject: Project | null;
  recentProjects: string[];
  setCurrentProject: (project: Project | null) => void;
  setRecentProjects: (paths: string[]) => void;
  addRecentProject: (path: string) => void;
}

export const useProjectStore = create<ProjectStore>((set) => ({
  currentProject: null,
  recentProjects: [],

  setCurrentProject: (project) => set({ currentProject: project }),
  setRecentProjects: (recentProjects) => set({ recentProjects }),

  addRecentProject: (path) =>
    set((state) => ({
      recentProjects: [path, ...state.recentProjects.filter((p) => p !== path)].slice(0, 10),
    })),
}));
