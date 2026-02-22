import { create } from "zustand";

export interface Project {
  name: string;
  filePath?: string;
  connectionIds: string[];
  dashboardOpen: boolean;
}

interface ProjectStore {
  currentProject: Project | null;
  recentProjects: string[];
  setCurrentProject: (project: Project | null) => void;
  addRecentProject: (path: string) => void;
}

export const useProjectStore = create<ProjectStore>((set) => ({
  currentProject: null,
  recentProjects: [],

  setCurrentProject: (project) => set({ currentProject: project }),

  addRecentProject: (path) =>
    set((state) => ({
      recentProjects: [path, ...state.recentProjects.filter((p) => p !== path)].slice(0, 10),
    })),
}));
