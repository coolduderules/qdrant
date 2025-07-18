"""Data models for e2e tests."""
from typing import Optional, List, Dict, Any, Union
from dataclasses import dataclass, field

import docker.models.containers
import docker.models.networks


class QdrantContainer:
    """Container info object for Qdrant containers."""
    
    def __init__(self, container: docker.models.containers.Container, host: str, name: str, 
                 http_port: int, grpc_port: Optional[int], compose_project: Optional[str] = None) -> None:
        self.container = container
        self.host = host
        self.name = name
        self.http_port = http_port
        self.grpc_port = grpc_port
        self.compose_project = compose_project
    
    def __repr__(self) -> str:
        return f"QdrantContainer(name='{self.name}', host='{self.host}', http_port={self.http_port}, grpc_port={self.grpc_port})"


class QdrantCluster:
    """Cluster info object for Qdrant clusters."""
    
    def __init__(self, leader: QdrantContainer, followers: List[QdrantContainer], 
                 network: docker.models.networks.Network, network_name: str) -> None:
        self.leader = leader
        self.followers = followers
        self.all_nodes = [leader] + followers
        self.network = network
        self.network_name = network_name
    
    def __repr__(self) -> str:
        return f"QdrantCluster(leader={self.leader.name}, followers={len(self.followers)}, network='{self.network_name}')"


@dataclass
class QdrantContainerConfig:
    """Configuration for creating Qdrant containers with proper typing and defaults."""
    
    # Container identification
    name: Optional[str] = None
    
    # Resource limits
    mem_limit: Optional[str] = None
    cpu_limit: Optional[str] = None
    
    # Network configuration
    network: Optional[str] = None
    network_mode: Optional[str] = None
    
    # Storage configuration
    volumes: Optional[Dict[str, Dict[str, str]]] = None
    mounts: Optional[List[Any]] = None
    
    # Environment and command
    environment: Optional[Dict[str, str]] = None
    command: Optional[Union[str, List[str]]] = None
    
    # Container behavior
    remove: bool = True
    detach: bool = True
    
    # Qdrant-specific settings
    exit_on_error: bool = True
    
    # Additional Docker parameters
    additional_params: Optional[Dict[str, Any]] = field(default_factory=dict)
    
    def to_docker_config(self, qdrant_image: str) -> Dict[str, Any]:
        """Convert this config to Docker container.run() parameters."""
        config = {
            "image": qdrant_image,
            "detach": self.detach,
            "remove": self.remove,
        }
        
        # Add port bindings unless host network mode
        if self.network_mode != 'host':
            config["ports"] = {'6333/tcp': ('127.0.0.1', None), '6334/tcp': ('127.0.0.1', None)}
        
        # Add optional parameters
        if self.name:
            config["name"] = self.name
        if self.mem_limit:
            config["mem_limit"] = self.mem_limit
        if self.cpu_limit:
            config["cpu_limit"] = self.cpu_limit
        if self.network:
            config["network"] = self.network
        if self.network_mode:
            config["network_mode"] = self.network_mode
        if self.volumes:
            config["volumes"] = self.volumes
        if self.mounts:
            config["mounts"] = self.mounts
        if self.environment:
            config["environment"] = self.environment
        if self.command:
            config["command"] = self.command
        
        # Add any additional parameters
        if self.additional_params:
            config.update(self.additional_params)
        
        return config