use crate::Cli;
use anyhow::Result;
use colored::{ColoredString, Colorize};
use duct::cmd;
use std::collections::{HashMap, HashSet, LinkedList};
use std::path::PathBuf;
use walkdir::WalkDir;

#[derive(Debug, Clone, derive_more::From, Hash, Eq, PartialEq, Ord, PartialOrd)]
pub struct Commit(String);

pub struct CommitDisplay<'a>(&'a Commit, &'a Repository);

impl std::fmt::Display for CommitDisplay<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.1.name(self.0))
    }
}

#[derive(Debug)]
pub struct Repository {
    pub directory: PathBuf,
    pub branch_names: Vec<String>,
    pub id_to_branches: HashMap<Commit, HashSet<String>>,
    pub nodes_to_children: HashMap<Commit, HashSet<Commit>>,
    pub nodes_to_parents: HashMap<Commit, HashSet<Commit>>,
    pub merge_bases: HashMap<(Commit, Commit), Commit>,
}

impl TryFrom<Cli> for Repository {
    type Error = anyhow::Error;

    fn try_from(cli: Cli) -> Result<Self> {
        let directory = if let Some(dir) = cli.directory {
            if !dir.is_dir() {
                anyhow::bail!("Not a directory: {:?}", dir);
            }
            dir
        } else {
            std::env::current_dir()?
        };

        let mut repo = Repository::new(directory)?;

        for branch in cli.branches {
            repo.add_branch(branch)?;
        }

        Ok(repo)
    }
}

impl Repository {
    pub fn new(directory: PathBuf) -> Result<Self> {
        let refs_dir = directory.join(".git/refs/heads");
        if !refs_dir.is_dir() {
            anyhow::bail!("Not a git directory: {:?}", directory);
        }

        Ok(Repository {
            directory,
            branch_names: Default::default(),
            id_to_branches: Default::default(),
            nodes_to_children: Default::default(),
            nodes_to_parents: Default::default(),
            merge_bases: Default::default(),
        })
    }

    pub fn run(&mut self) -> Result<()> {
        if self.branch_names.is_empty() {
            self.read_branches()?;
        }

        let mut new_nodes = self.id_to_branches.keys().cloned().collect::<LinkedList<_>>();
        for node in &new_nodes {
            self.nodes_to_children
                .insert(node.clone(), Default::default());
            self.nodes_to_parents
                .insert(node.clone(), Default::default());
        }

        while let Some(new_node) = new_nodes.pop_front() {
            let keys = self.nodes_to_children.keys().cloned().collect::<Vec<_>>();
            for node in keys {
                let base = self.merge_base(&new_node, &node)?;

                if !self.nodes_to_children.contains_key(&base) {
                    self.nodes_to_children
                        .insert(base.clone(), Default::default());
                    new_nodes.push_back(base.clone());
                }

                if let Some(children) = self.nodes_to_children.get_mut(&base) {
                    if base != node {
                        children.insert(node.clone());
                    }
                    if base != new_node {
                        children.insert(new_node.clone());
                    }
                }

                if !self.nodes_to_parents.contains_key(&node) {
                    self.nodes_to_parents
                        .insert(node.clone(), Default::default());
                }
                if node != base {
                    if let Some(parents) = self.nodes_to_parents.get_mut(&node) {
                        parents.insert(base.clone());
                    }
                }

                if !self.nodes_to_parents.contains_key(&new_node) {
                    self.nodes_to_parents
                        .insert(new_node.clone(), Default::default());
                }
                if new_node != base {
                    if let Some(parents) = self.nodes_to_parents.get_mut(&new_node) {
                        parents.insert(base.clone());
                    }
                }
            }
        }

        let Some(oldest) = self
            .nodes_to_parents
            .iter()
            .find(|(_, parents)| parents.is_empty())
            .map(|(oldest, _)| oldest)
            .cloned()
        else {
            anyhow::bail!("Unable to determine ultimate parent node");
        };

        self.prune_children(oldest.clone());

        let mut leaves = self
            .nodes_to_children
            .iter()
            .filter_map(|(node, children)| {
                if children.is_empty() {
                    Some(node)
                } else {
                    None
                }
            })
            .cloned()
            .collect::<Vec<_>>();

        leaves.sort_by_key(|leaf| self.nodes_to_parents.get(leaf).map(|p| p.len()));

        for leaf in &leaves {
            self.prune_parents(leaf.clone());
        }

        println!("digraph {{");
        let mut nodes_to_id = HashMap::<Commit, usize>::new();
        for node in self.nodes_to_children.keys() {
            let id = nodes_to_id.len();
            nodes_to_id.insert(node.clone(), id);
            println!("\t{} [label=\"{}\"]", id, self.name(node));
        }
        for (node, children) in self.nodes_to_children.iter() {
            for child in children {
                println!("\t{} -> {}", nodes_to_id[node], nodes_to_id[child]);
            }
        }
        println!("}}");

        Ok(())
    }

    fn prune_children(&mut self, parent: Commit) {
        let mut children = self.nodes_to_children.get(&parent).unwrap().clone();
        let all_children = children.clone();
        children.retain(|node| {
            for child in &all_children {
                if child == node {
                    continue;
                }
                if self.nodes_to_children.get(child).unwrap().contains(node) {
                    return false;
                }
            }
            true
        });

        self.nodes_to_children.insert(parent, children);

        for child in all_children {
            self.prune_children(child);
        }
    }

    fn prune_parents(&mut self, child: Commit) {
        let mut parents = self.nodes_to_parents.get(&child).unwrap().clone();
        let all_parents = parents.clone();

        parents.retain(|node| {
            for parent in &all_parents {
                if parent == node {
                    continue;
                }
                if self.nodes_to_parents.get(parent).unwrap().contains(node) {
                    return false;
                }
            }
            true
        });

        self.nodes_to_parents.insert(child, parents);

        for parent in all_parents {
            self.prune_parents(parent);
        }
    }

    fn merge_base(&mut self, lhs: &Commit, rhs: &Commit) -> Result<Commit> {
        let (lhs, rhs) = if rhs > lhs { (rhs, lhs) } else { (lhs, rhs) };

        if let Some(commit) = self.merge_bases.get(&(lhs.clone(), rhs.clone())) {
            Ok(commit.clone())
        } else {
            let value = cmd!(
                "git",
                "-C",
                self.directory.as_os_str(),
                "merge-base",
                lhs.0.as_str(),
                rhs.0.as_str(),
            )
            .read()?;
            let commit = Commit(value);
            self.merge_bases
                .insert((lhs.clone(), rhs.clone()), commit.clone());

            Ok(commit)
        }
    }

    fn name(&self, commit: &Commit) -> ColoredString {
        let hash = commit.0.as_str()[0..9].red();
        if let Some(names) = self.id_to_branches.get(commit) {
            format!(
                "{} {}",
                hash,
                names.iter().map(|name| format!("{}", name.as_str().green())).collect::<Vec<_>>().join(", "),
            ).into()
        } else {
            hash
        }
    }

    fn read_branches(&mut self) -> Result<()> {
        let refs_dir = self.directory.join(".git/refs/heads");

        for entry in WalkDir::new(&refs_dir).into_iter().filter_map(|e| e.ok()) {
            if entry.path().is_file() {
                if let Ok(Some(name)) = entry.path().strip_prefix(&refs_dir).map(|p| p.to_str()) {
                    self.add_branch(name)?;
                }
            }
        }

        Ok(())
    }

    fn add_branch<T: ToString>(&mut self, branch: T) -> Result<()> {
        let branch = branch.to_string();
        log::debug!("add_branch: {:?}", &branch);
        let branch_path = self
            .directory
            .join(".git/refs/heads/")
            .join(branch.as_str());
        let id = std::fs::read_to_string(branch_path)?.trim().to_string();
        self.branch_names.push(branch.clone());
        self.id_to_branches.entry(id.clone().into()).or_default().insert(branch.clone());

        Ok(())
    }
}
