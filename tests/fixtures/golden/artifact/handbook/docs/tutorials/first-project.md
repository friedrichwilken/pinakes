# Create your first project

Create a project with the command line, add a member and upload a file.

## Create the project

```sh
service project create demo
```

## Add a member

```sh
service project add-member demo alice --role editor
```

## Upload a file

```sh
service upload demo ./notes.txt
```
