QT += core gui qml quick quickcontrols2 printsupport dbus network

CONFIG += c++17 release
TARGET = icloud-notes
TEMPLATE = app

# `icloud-notes --version`; the package build passes its version.
NOTES_VERSION = $$(ICLOUD_NOTES_VERSION)
isEmpty(NOTES_VERSION): NOTES_VERSION = dev
DEFINES += ICLOUD_NOTES_VERSION=\\\"$$NOTES_VERSION\\\"

HEADERS += \
    src/backgroundsync.h \
    src/cli.h \
    src/notesbackend.h \
    src/singleinstance.h \
    src/vaultlock.h \
    src/markdownhighlighter.h

SOURCES += \
    src/main.cpp \
    src/backgroundsync.cpp \
    src/cli.cpp \
    src/notesbackend.cpp \
    src/singleinstance.cpp \
    src/markdownhighlighter.cpp

RESOURCES += qml/resources.qrc
